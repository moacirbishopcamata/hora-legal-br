//! Resiliencia: retentativa com *jitter*, disjuntor e cache de calibracao.
//!
//! Tres responsabilidades, tres tipos, zero conhecimento mútuo:
//!
//! | Tipo | Pergunta que responde | Conhece o relogio? |
//! |---|---|---|
//! | [`Espera`] | "quanto tempo eu espero antes da proxima tentativa?" | nao |
//! | [`Disjuntor`] | "esta fonte merece ser tentada agora?" | nao |
//! | [`Calibracao`] | "a ultima amostra ainda vale?" | nao |
//!
//! Todos recebem `agora` como **parametro** em vez de chamar o relogio. E' o
//! que torna a resiliencia testavel sem `sleep` e sem *mocks* -- o
//! parametro `agora` e' a costura.
//!
//! ## Resumo das pre-condicoes e invariantes
//!
//! **Pre-condicoes:**
//! - [`Espera::nova`]: `teto >= inicial > 0`, `teto <= 30 s`;
//! - [`Disjuntor::novo`]: `falhas_para_abrir >= 2`, `recuperacao <= 60 s`;
//! - [`Calibracao::de_amostra`]: a amostra ja passou pelas pre-condicoes de
//!   [`crate::amostra`].
//!
//! **Invariantes** (verificadas com `panic!`, para sempre):
//! - `Espera`: `esperar(n)` e' sempre `<= teto`, para todo `n`;
//! - `Disjuntor`: `tentativas == 0` sempre que o estado e' `Fechado`;
//! - `Calibracao`: uma amostra rejeitada **jamais** substitui a vigente.
//!
//! **Pos-condicoes:**
//! - `Espera::esperar(n) >= 0` (nunca negativo, mesmo com jitter adverso);
//! - `Disjuntor::permitir_tentativa` nao tem efeito colateral;
//! - `Calibracao::corrigir` devolve `agora + desvio` da amostra vigente.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::amostra::AmostraDeTempo;
use crate::exigir;
use crate::instante::InstanteUtc;
use crate::relogio::{Relogio, Sorteio};

/// Teto de espera aceitavel numa retentativa. Alem disso a API responde mais
/// rapido falhando do que respondendo.
pub const ESPERA_MAXIMA_ACEITAVEL: Duration = Duration::from_secs(30);

/// Espera antes da proxima tentativa: exponencial, com *jitter*.
///
/// ## A formula e por que cada parte existe
///
/// ```text
/// teto_da_tentativa = min(inicial * 2^n, teto)
/// espera           = teto_da_tentativa / 2 + real() * teto_da_tentativa / 2
/// ```
///
/// O *jitter* e' **sempre positivo** (`[teto/2, teto)`) em vez do classico
/// "full jitter" (`[0, teto)`). A razao e' operacional: se duas requisicoes
/// falharam juntas, elas receberam a mesma resposta de falha no mesmo
/// instante; com espera zero, as duas voltariam a bater na mesma fonte no
/// mesmo milissegundo -- sincronizando o problema em vez de espalhando.
/// Garantir um minimo de metade do intervalo quebra esse alinhamento sem
/// custar a propriedade que importa (espera cresce e respeita o teto).
#[derive(Debug, Clone, Copy)]
pub struct Espera {
    inicial: Duration,
    teto: Duration,
}

impl Espera {
    /// Constroi a politica de espera.
    ///
    /// **Pre-condicoes:** `inicial > 0`; `teto >= inicial`; `teto <= 30 s`.
    pub fn nova(inicial: Duration, teto: Duration) -> Result<Self, crate::contrato::ErroDeContrato> {
        use crate::contrato::ErroDeContrato as Erro;
        // As tres pre-condicoes como tres `match`: cada uma nomeia o motivo,
        // e nao existe caminho que as ignore.
        if inicial.is_zero() {
            return Err(Erro::novo("Espera::nova", "inicial precisa ser > 0"));
        }
        if teto < inicial {
            return Err(Erro::novo("Espera::nova", "teto precisa ser >= inicial"));
        }
        if teto > ESPERA_MAXIMA_ACEITAVEL {
            return Err(Erro::novo(
                "Espera::nova",
                "teto acima de 30s transforma retentativa em timeout",
            ));
        }
        Ok(Self { inicial, teto })
    }

    /// Tempo a esperar antes da tentativa de numero `n` (zero-based).
    ///
    /// **Pos-condicoes:** resultado `>= inicial/2` e `<= teto`.
    /// O deslocamento de `n` satura em 63 para nao estourar `Duration`.
    pub fn esperar(&self, n: u32, sorteio: &dyn Sorteio) -> Duration {
        // 2^63 nanos de overflow avoided: saturating_shift_left no u64 de nanos.
        let base_nanos = u64::try_from(self.inicial.as_nanos()).unwrap_or(u64::MAX);
        let teto_nanos = u64::try_from(self.teto.as_nanos()).unwrap_or(u64::MAX);
        let escalado = base_nanos
            .checked_shl(n.min(63))
            .unwrap_or(u64::MAX)
            .min(teto_nanos);
        let metade = escalado / 2;
        // `real()` < 1 por pre-condicao, entao a soma fica < teto. O `clamp`
        // torna a invariante defensiva: um Sorteio mal-comportado nao pode
        // furar o teto nem produzir espera negativa.
        let jitter = sorteio.real().clamp(0.0, 1.0) * (escalado - metade) as f64;
        let total = metade + jitter as u64;
        exigir!(
            total <= teto_nanos,
            "Espera::esperar",
            "espera {total}ns excedeu o teto {teto_nanos}ns"
        );
        Duration::from_nanos(total)
    }
}

/// Estado de um [`Disjuntor`].
///
/// ### Por que os campos sao parte do estado, e nao um metodo
///
/// `Aberto { ate }` carrega **quando** o disjuntor fecha. Sem esse dado, o
/// chamador teria de recalcular o cooldown -- e qualquer divergencia ali
/// viraria um disjuntor que nunca abre ou nunca fecha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstadoDoDisjuntor {
    /// Saudavel: tentativas fluem normalmente.
    Fechado,
    /// Aberto: falhou demais. Nenhuma tentativa passa ate `ate`.
    Aberto {
        /// Instante em que a cooldown termina.
        ate: InstanteUtc,
    },
    /// Semioberto: uma unica tentativa de sondagem passa, para testar se a
    /// fonte voltou.
    EmRecuperacao {
        /// Instante da tentativa em voo, ou `None` se nenhuma foi reservada.
        tentado_em: Option<InstanteUtc>,
    },
}

/// Disjuntor de fonte.
///
/// ## Pre-condicoes
/// - `falhas_para_abrir >= 2` (abrir com uma falha reativa a um blip de rede);
/// - `recuperacao <= 60 s` (recuperacao mais longa que isso e' Lambdase, e
///   o processo precisa reiniciar para recuperar).
///
/// ## Invariante
/// `estado == Fechado` implica `tentativas == 0`. Sem isso, o disjuntor
/// reabriria na proxima falha mesmo apos um sucesso, e o teste
/// `disjuntor_verde_tem_contador_zero` existe para travar isso.
///
/// ## Por que o tempo entra por parametro
///
/// `permitir_tentativa(&self, agora)` e pura. Isso permite testar a
/// transicao `Aberto -> EmRecuperacao -> Fechado` com tres linhas, sem
/// `sleep`, sem relogio mockado, sem flakiness. O custo e' que o chamador
/// precisa ter um relogio -- e ele sempre tem, porque ja o passou para a
/// camada de cima.
#[derive(Debug)]
pub struct Disjuntor {
    falhas_para_abrir: u32,
    recuperacao: Duration,
    falhas: AtomicU64,
    aberto_ate: AtomicU64,
    /// Tentativa em curso de recuperacao, em nanos desde o epoch.
    tentativa_em: AtomicU64,
    /// `true` quando `tentativa_em` vale.
    em_tentativa: AtomicU64,
}

impl Disjuntor {
    /// Constroi o disjuntor.
    ///
    /// **Pre-condicoes:** ver doc do tipo.
    pub const fn novo(falhas_para_abrir: u32, recuperacao: Duration) -> Self {
        Self {
            falhas_para_abrir,
            recuperacao,
            falhas: AtomicU64::new(0),
            aberto_ate: AtomicU64::new(0),
            tentativa_em: AtomicU64::new(0),
            em_tentativa: AtomicU64::new(0),
        }
    }

    /// Estado atual no instante `agora`.
    pub fn estado(&self, agora: InstanteUtc) -> EstadoDoDisjuntor {
        let falhas = self.falhas.load(Ordering::Acquire);
        if falhas < u64::from(self.falhas_para_abrir) {
            return EstadoDoDisjuntor::Fechado;
        }
        let limite = InstanteUtc::de_relogio_do_host(
            i64::try_from(self.aberto_ate.load(Ordering::Acquire)).unwrap_or(0),
            0,
        );
        // Regra de decisao do disjuntor, em um unico `match`. A ordem dos bracos e'
        // a ordem da prioridade: fechado > aberto > em recuperacao.
        //
        // `delta_ate` e' com sinal, entao um valor **negativo** significa
        // "o cooldown ainda nao venceu". Sem `if`: os tres estados sao variantes
        // de um enum, e o compilador garante que os tres casos existem.
        let cooldown_vencido = !limite.delta_ate(agora).is_negative();
        let em_tentativa = self.em_tentativa.load(Ordering::Acquire) == 1;
        let instante_da_tentativa = InstanteUtc::de_relogio_do_host(
            i64::try_from(self.tentativa_em.load(Ordering::Acquire)).unwrap_or(0),
            0,
        );
        match (cooldown_vencido, em_tentativa) {
            (false, _) => EstadoDoDisjuntor::Aberto { ate: limite },
            (true, true) => EstadoDoDisjuntor::EmRecuperacao {
                tentado_em: Some(instante_da_tentativa),
            },
            (true, false) => EstadoDoDisjuntor::EmRecuperacao { tentado_em: None },
        }
    }

    /// Decide se uma tentativa pode passar agora.
    ///
    /// **Pos-condicao:** sem efeito colateral. Quem chama precisa, depois,
    /// invocar [`Self::registrar_sucesso`] ou [`Self::registrar_falha`] para
    /// que o disjuntor evolua.
    pub fn permitir_tentativa(&self, agora: InstanteUtc) -> bool {
        match self.estado(agora) {
            EstadoDoDisjuntor::Fechado => true,
            EstadoDoDisjuntor::Aberto { .. } => false,
            // So uma tentativa de recuperacao por vez: se ja ha uma em voo,
            // a proxima espera. Sem isso, um pico de trafego abriria N
            // sondagens contra a fonte que acabou de cair.
            EstadoDoDisjuntor::EmRecuperacao { tentado_em } => tentado_em.is_none(),
        }
    }

    /// Reserva a vaga de recuperacao. Deve ser chamado logo apos
    /// `permitir_tentativa` retornar `true` num estado de recuperacao.
    pub fn reservar_tentativa(&self, agora: InstanteUtc) {
        self.tentativa_em
            .store(u64::try_from(agora.segundos_desde_epoch()).unwrap_or(0), Ordering::Release);
        self.em_tentativa.store(1, Ordering::Release);
    }

    /// Registro de sucesso: fecha o disjuntor.
    ///
    /// **Invariante:** depois desta chamada, `falhas == 0`.
    pub fn registrar_sucesso(&self, agora: InstanteUtc) {
        self.falhas.store(0, Ordering::Release);
        self.em_tentativa.store(0, Ordering::Release);
        self.aberto_ate.store(0, Ordering::Release);
        let _ = agora;
    }

    /// Registro de falha: incrementa o contador e talvez abra o disjuntor.
    ///
    /// **Pos-condicao:** se o contador alcancar o limite, `aberto_ate` passa a
    /// valer `agora + recuperacao`, e a proxima leitura de `estado` devolve
    /// `Aberto`.
    pub fn registrar_falha(&self, agora: InstanteUtc) {
        self.em_tentativa.store(0, Ordering::Release);
        let falhas = self.falhas.fetch_add(1, Ordering::AcqRel) + 1;
        if falhas >= u64::from(self.falhas_para_abrir) {
            let limite = agora.mais(self.recuperacao);
            self.aberto_ate.store(
                u64::try_from(limite.segundos_desde_epoch()).unwrap_or(0),
                Ordering::Release,
            );
        }
    }

    /// Contador de falhas consecutivas.
    #[must_use]
    pub fn falhas(&self) -> u64 {
        self.falhas.load(Ordering::Acquire)
    }
}

/// Calibracao vigente: a ultima amostra confiavel, com metadados de idade.
///
/// ## A invariante central
///
/// **Uma amostra rejeitada jamais substitui a vigente.** Sem isso, uma falha
/// momentanea faria o servico perder a referencia e cair no relogio local --
/// que e precisamente o relogio que o cliente esta tentando corrigir. E' o
/// pior erro possivel num servico de tempo, e e' silencioso.
#[derive(Debug)]
pub struct Calibracao {
    amostra: Mutex<AmostraDeTempo>,
    publicada_em: Mutex<InstanteUtc>,
    /// `true` quando a amostra foi rebaixada por perda de fonte.
    degradada: Mutex<bool>,
    /// `true` so depois de uma medicao **real**. Ver [`Calibracao::sem_medida`].
    medida: Mutex<bool>,
}

impl Calibracao {
    /// Cria a calibracao a partir de uma amostra ja validada.
    ///
    /// **Pre-condicao:** `amostra` passou por [`crate::amostra::AmostraDeTempo::de_bruta`].
    /// **Pos-condicao:** idade zero no instante de publicacao.
    #[must_use]
    pub fn de_amostra(amostra: AmostraDeTempo, agora: InstanteUtc) -> Self {
        Self {
            amostra: Mutex::new(amostra),
            publicada_em: Mutex::new(agora),
            degradada: Mutex::new(false),
            medida: Mutex::new(true),
        }
    }

    /// Calibracao **sem nenhuma medicao real**: o estado de quem acabou de subir.
    ///
    /// ## Por que existe
    ///
    /// Servir uma amostra inventada (a de teste, com +5 ms) como se fosse
    /// calibracao deixaria as primeiras respostas do servico com desvio
    /// fabricado — e o cliente de um servico de hora legal nao tem como
    /// perceber. Aqui o estado e' explicito: `tem_medida() == false`.
    ///
    /// **Invariantes:**
    /// - `decidir` nunca devolve `UsarCache` para uma calibracao sem medida;
    /// - o caso de uso nunca serve, nem degradado, uma calibracao sem medida;
    /// - `publicar` e' o unico caminho para `tem_medida() == true`.
    ///
    /// A amostra interna e' apenas um valor de preenchimento do tipo e **nao
    /// e' lida** por nenhum caminho de resposta.
    #[must_use]
    pub fn sem_medida(agora: InstanteUtc) -> Self {
        let calibracao = Self::de_amostra(crate::fixture::amostra_de_teste(), agora);
        *Self::travar(&calibracao.medida) = false;
        *Self::travar(&calibracao.degradada) = true;
        calibracao
    }

    /// Trava o mutex **mesmo envenenado**.
    ///
    /// Um `panic!` em outra thread envenena o lock, mas os dados guardados
    /// aqui sao valores simples (copiaveis), sempre coerentes entre uma
    /// escrita e outra. Recusar a leitura derrubaria o servico por um problema
    /// que nao corrompeu nada; recuperar o conteudo e' o comportamento
    /// resiliente correto.
    fn travar<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `true` se ja houve ao menos uma medicao real.
    #[must_use]
    pub fn tem_medida(&self) -> bool {
        *Self::travar(&self.medida)
    }

    /// Publica uma nova amostra.
    ///
    /// **Pos-condicoes:**
    /// 1. a calibracao passa a refletir `amostra`;
    /// 2. `degradada` volta a `false`.
    pub fn publicar(&self, amostra: AmostraDeTempo, agora: InstanteUtc) {
        if let Ok(mut g) = self.amostra.lock() {
            *g = amostra;
        }
        if let Ok(mut g) = self.publicada_em.lock() {
            *g = agora;
        }
        if let Ok(mut g) = self.degradada.lock() {
            *g = false;
        }
        *Self::travar(&self.medida) = true;
    }

    /// Marca a calibracao como degradada (usada quando todas as fontes falham).
    pub fn rebaixar(&self) {
        if let Ok(mut g) = self.degradada.lock() {
            *g = true;
        }
    }

    /// `true` se a calibracao esta sendo servida em modo degradado.
    #[must_use]
    pub fn esta_degradada(&self) -> bool {
        self.degradada.lock().is_ok_and(|g| *g)
    }

    /// Amostra vigente.
    ///
    /// **Pos-condicao:** devolve sempre `Some` apos a construcao. Uma
    /// calibracao sem amostra nao e' um estado que o tipo represente --
    /// `Option` aqui seria *null* (Yegor256 #1) para um caso que nao ocorre.
    #[must_use]
    pub fn amostra(&self) -> AmostraDeTempo {
        *Self::travar(&self.amostra)
    }

    /// Instante da ultima publicacao (ou da criacao, se nunca houve).
    #[must_use]
    pub fn publicada_em(&self) -> InstanteUtc {
        match self.publicada_em.lock() {
            Ok(guarda) => *guarda,
            Err(_) => InstanteUtc::epoch(),
        }
    }

    /// Idade da calibracao no instante `agora`.
    pub fn idade(&self, agora: InstanteUtc) -> Result<Duration, crate::contrato::ErroDeContrato> {
        self.publicada_em().idade(agora)
    }

    /// Corrige `relogio` pela calibracao vigente.
    ///
    /// **Pos-condicao:** devolve `relogio + desvio`, com erro limitado pela
    /// `politica` que validou a amostra.
    #[must_use]
    pub fn corrigir(&self, relogio: InstanteUtc) -> InstanteUtc {
        self.amostra().corrigir(relogio)
    }
}

impl Clone for Calibracao {
    fn clone(&self) -> Self {
        Self {
            amostra: Mutex::new(self.amostra()),
            publicada_em: Mutex::new(self.publicada_em()),
            degradada: Mutex::new(self.esta_degradada()),
            medida: Mutex::new(self.tem_medida()),
        }
    }
}

/// Idade maxima de uma calibracao para ser servida **em modo degradado**.
///
/// Escolha de engenharia, nao norma: um oscilador de quartzo comum deriva da
/// ordem de 50 ppm, o que da ~45 ms em 15 minutos. Passado isso, a hora
/// corrigida deixa de merecer o nome de hora legal e o servico prefere
/// responder `503` a mentir com boa aparencia.
pub const IDADE_MAXIMA_DEGRADADA: Duration = Duration::from_secs(15 * 60);

/// Reune os tres mecanismos para uma fonte.
///
/// Faixa de latencia assumida: `consulta` prefere a calibracao em cache
/// enquanto ela estiver dentro da janela; senao sonda.
#[derive(Debug)]
pub struct FonteResiliente {
    nome: String,
    espera: Espera,
    disjuntor: Disjuntor,
    calibracao: Calibracao,
}

impl FonteResiliente {
    /// Constroi a fonte.
    ///
    /// **Pre-condicoes:** `nome` nao vazio; ver [`Espera::nova`] e
    /// [`Disjuntor::novo`] para as demais.
    pub fn nova(
        nome: impl Into<String>,
        espera: Espera,
        disjuntor: Disjuntor,
        calibracao: Calibracao,
    ) -> Self {
        let nome = nome.into();
        exigir!(
            !nome.trim().is_empty(),
            "FonteResiliente::nova",
            "nome vazio impede log e resposta de diagnostico"
        );
        Self {
            nome,
            espera,
            disjuntor,
            calibracao,
        }
    }

    /// Nome da fonte.
    #[must_use]
    pub fn nome(&self) -> &str {
        &self.nome
    }

    /// Espera antes da proxima tentativa.
    #[must_use]
    pub const fn espera(&self) -> &Espera {
        &self.espera
    }

    /// Disjuntor associado.
    #[must_use]
    pub const fn disjuntor(&self) -> &Disjuntor {
        &self.disjuntor
    }

    /// Calibracao associada.
    #[must_use]
    pub const fn calibracao(&self) -> &Calibracao {
        &self.calibracao
    }

    /// Fluxo de decisao de uma consulta, no instante `agora`.
    ///
    /// **Pos-condicao:** responde a pergunta completa sem tocar a rede,
    /// para que o chamador decida se sonda ou servecaIh cache.
    pub fn decidir(&self, agora: InstanteUtc, janela: Duration) -> DecisaoDeConsulta {
        let idade = self.calibracao.idade(agora).unwrap_or(Duration::MAX);
        let dentro_da_janela = idade <= janela;
        match self.disjuntor.estado(agora) {
            EstadoDoDisjuntor::Aberto { .. } => DecisaoDeConsulta::FonteIndisponivel {
                calibra_em: self.calibracao.publicada_em(),
            },
            _ if dentro_da_janela && self.calibracao.tem_medida() && !self.calibracao.esta_degradada() => DecisaoDeConsulta::UsarCache {
                calibracao: self.calibracao.clone(),
            },
            _ => DecisaoDeConsulta::Sondar {
                tentativa: u32::try_from(self.disjuntor.falhas()).unwrap_or(u32::MAX),
            },
        }
    }
}

/// O que fazer numa consulta, ja decidido.
///
/// ### O enum **e'** a politica de latencia
///
/// Estas tres variantes sao o conjunto completo de decisoes do caminho
/// rapido. Nenhuma delas "e' uma fonte e' outra fonte": quem escolhe a
/// fonte seguinte e' o caso de uso. Aqui decide-se apenas **se a rede e'
/// tocada**.
#[derive(Debug, Clone)]
pub enum DecisaoDeConsulta {
    /// Serve a calibracao em cache, sem tocar a rede.
    UsarCache {
        /// Calibracao a servir.
        calibracao: Calibracao,
    },
    /// Faz uma sondagem de rede.
    Sondar {
        /// Numero de falhas ate agora, para calcular o *backoff*.
        tentativa: u32,
    },
    /// A fonte esta fora do ar; se houver calibracao, ela e' servida degradada.
    FonteIndisponivel {
        /// Momento da ultima calibracao conhecida.
        calibra_em: InstanteUtc,
    },
}

impl DecisaoDeConsulta {
    /// `true` se a decisao evita a rede.
    ///
    /// Este metodo existe para que o teste de latencia possa *afirmar* a
    /// ausencia de I/O, em vez de inferir.
    #[must_use]
    pub const fn evita_rede(&self) -> bool {
        matches!(self, Self::UsarCache { .. } | Self::FonteIndisponivel { .. })
    }
}

/// Atalho para construir um disjuntor com os padroes do servico.
///
/// Um `fn` de fabricacao, nao um "utility class": e' o unico ponto onde os
/// numeros magos do disjuntor vivem.
#[must_use]
pub fn disjuntor_padrao() -> Disjuntor {
    Disjuntor::novo(3, Duration::from_secs(10))
}

/// Atalho de fabricacao para a politica de espera padrao.
pub fn espera_padrao() -> Result<Espera, crate::contrato::ErroDeContrato> {
    Espera::nova(Duration::from_millis(8), Duration::from_millis(50))
}

/// Fabrica uma calibracao a partir de uma amostra e do relogio.
///
/// Existe para que a construcao em producao e em teste sejam **a mesma
/// linha de codigo** (G27 *Structure over Convention*): nao ha duas formas de
/// criar calibracao, e portanto nao ha duas formas de errar.
pub fn calibracao_de(
    amostra: AmostraDeTempo,
    relogio: &dyn Relogio,
) -> Calibracao {
    Calibracao::de_amostra(amostra, relogio.agora())
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::fixture::politica_de_teste;
    use crate::amostra::{Estrato, LeiturasBrutas};
    use crate::relogio::{RelogioManual, SorteioFixo};

    /// Espera de teste: 10 ms a 160 ms.
    ///
    /// O teto de 160 ms e' escolha, nao acaso: com 10 ms de base, 160 ms
    /// significa 4 dobras (10, 20, 40, 80, 160), entao o teste
    /// `espera_exponencial_respeita_o_teto` exercita **regime** alem do
    /// saturado.
    fn espera_de_teste() -> Espera {
        crate::fixture::de_ou_panica(
            Espera::nova(Duration::from_millis(10), Duration::from_millis(160)),
            "espera de teste (10ms..160ms)",
        )
    }

    #[test]
    fn espera_exponencial_respeita_o_teto() {
        let espera = espera_de_teste();
        let sorteio = SorteioFixo::novo(0.999_999);
        // Teto de 160ms nunca pode ser ultrapassado, em nenhuma tentativa.
        for n in 0..20 {
            let d = espera.esperar(n, &sorteio);
            exigir!(
                d <= Duration::from_millis(160),
                "invariante Espera: tentativa {n} excedeu o teto",
                "{d:?}"
            );
        }
    }

    #[test]
    fn espera_cresce_com_a_tentativa() {
        let espera = espera_de_teste();
        let sorteio = SorteioFixo::novo(0.5);
        let primeira = espera.esperar(0, &sorteio);
        let segunda = espera.esperar(1, &sorteio);
        let terceira = espera.esperar(2, &sorteio);
        exigir!(
            segunda > primeira && terceira > segunda,
            "pos-condicao: espera cresce exponencialmente",
            "{primeira:?} {segunda:?} {terceira:?}"
        );
    }

    #[test]
    fn espera_nunca_abaixo_da_metade_do_intervalo() {
        // E' a propriedade que impede dois clientes de voltarem juntos.
        let espera = espera_de_teste();
        let sorteio = SorteioFixo::novo(0.0);
        for n in 0..8 {
            let d = espera.esperar(n, &sorteio);
            exigir!(
                d >= Duration::from_millis(5),
                "jitter nunca zera a esperanca de dispersao (n={n})",
                "{d:?}"
            );
        }
    }

    #[test]
    fn espera_invalida_e_rejeitada() {
        let r = Espera::nova(Duration::ZERO, Duration::from_millis(10));
        exigir!(r.is_err(), "pre-condicao: inicial zero", "{r:?}");
        let r = Espera::nova(Duration::from_millis(100), Duration::from_millis(10));
        exigir!(r.is_err(), "pre-condicao: teto < inicial", "{r:?}");
        let r = Espera::nova(Duration::from_millis(1), Duration::from_secs(60));
        exigir!(r.is_err(), "pre-condicao: teto > 30s", "{r:?}");
    }

    #[test]
    fn disjuntor_permite_ate_o_limite_de_falhas() {
        let d = Disjuntor::novo(3, Duration::from_secs(10));
        let agora = InstanteUtc::epoch();
        d.registrar_falha(agora);
        d.registrar_falha(agora);
        exigir!(
            d.permitir_tentativa(agora),
            "invariante: 2 falhas < limite 3, ainda fechado",
            "{:?}",
            d.estado(agora)
        );
    }

    #[test]
    fn disjuntor_abre_ao_alcancar_o_limite() {
        let d = Disjuntor::novo(3, Duration::from_secs(10));
        let agora = InstanteUtc::epoch();
        for _ in 0..3 {
            d.registrar_falha(agora);
        }
        exigir!(
            !d.permitir_tentativa(agora),
            "pos-condicao: 3 falhas abre o disjuntor",
            "{:?}",
            d.estado(agora)
        );
    }

    #[test]
    fn disjuntor_verde_tem_contador_zero() {
        // Invariante do tipo: fechado <=> zero falhas.
        let d = Disjuntor::novo(3, Duration::from_secs(10));
        let agora = InstanteUtc::epoch();
        d.registrar_falha(agora);
        d.registrar_sucesso(agora);
        exigir!(d.falhas() == 0, "invariante: sucesso zera o contador", "{}", d.falhas());
        exigir!(
            d.estado(agora) == EstadoDoDisjuntor::Fechado,
            "estado coerente com o contador",
            "{:?}",
            d.estado(agora)
        );
    }

    #[test]
    fn disjuntor_recupera_apos_o_intervalo() {
        let d = Disjuntor::novo(2, Duration::from_secs(10));
        let inicio = InstanteUtc::epoch();
        d.registrar_falha(inicio);
        d.registrar_falha(inicio);
        exigir!(!d.permitir_tentativa(inicio), "aberto", "permitiu cedo demais");
        let depois = inicio.mais(Duration::from_secs(11));
        exigir!(
            matches!(d.estado(depois), EstadoDoDisjuntor::EmRecuperacao { tentado_em: None }),
            "apos o cooldown, em recuperacao",
            "{:?}",
            d.estado(depois)
        );
        d.registrar_sucesso(depois);
        exigir!(d.permitir_tentativa(depois), "sucesso fecha o disjuntor", "{:?}", d.estado(depois));
    }

    #[test]
    fn calibracao_idade_cresce_com_o_tempo() {
        let c = Calibracao::de_amostra(
            amostra_valida(),
            InstanteUtc::epoch(),
        );
        let i = c.idade(InstanteUtc::epoch());
        exigir!(
            i == Ok(Duration::ZERO),
            "pos-condicao: idade zero na publicacao",
            "{i:?}"
        );
        let depois = InstanteUtc::epoch().mais(Duration::from_secs(5));
        exigir!(
            c.idade(depois) == Ok(Duration::from_secs(5)),
            "idade correta apos 5s",
            "{:?}",
            c.idade(depois)
        );
    }

    #[test]
    fn calibracao_nao_perde_a_amostra_quando_rebaixada() {
        let relogio = RelogioManual::fixo(InstanteUtc::epoch());
        let c = calibracao_de(amostra_valida(), &relogio);
        let antes = c.amostra();
        c.rebaixar();
        exigir!(
            c.amostra() == antes,
            "invariante: rebaixar nao descarta a amostra vigente",
            "amostra mudou"
        );
        exigir!(c.esta_degradada(), "flag de degradacao setada", "nao");
    }

    #[test]
    fn fonte_dentro_da_janela_evita_a_rede() {
        // Este e' o teste que sustenta o requisito de latencia: com
        // calibracao fresca, a decisao e' servir do cache.
        let relogio = RelogioManual::fixo(InstanteUtc::epoch());
        let fonte = FonteResiliente::nova(
            "a.ntp.br",
            espera_de_teste(),
            disjuntor_padrao(),
            calibracao_de(amostra_valida(), &relogio),
        );
        let decisao = fonte.decidir(InstanteUtc::epoch(), Duration::from_millis(250));
        exigir!(
            matches!(decisao, DecisaoDeConsulta::UsarCache { .. }),
            "calibracao nova e' servida do cache",
            "{decisao:?}"
        );
        exigir!(decisao.evita_rede(), "pos-condicao: sem I/O", "{decisao:?}");
    }

    #[test]
    fn fonte_fora_da_janela_sonda() {
        let relogio = RelogioManual::fixo(InstanteUtc::epoch());
        let fonte = FonteResiliente::nova(
            "a.ntp.br",
            espera_de_teste(),
            disjuntor_padrao(),
            calibracao_de(amostra_valida(), &relogio),
        );
        let agora = InstanteUtc::epoch().mais(Duration::from_millis(300));
        let decisao = fonte.decidir(agora, Duration::from_millis(250));
        exigir!(
            matches!(decisao, DecisaoDeConsulta::Sondar { .. }),
            "calibracao vencida obriga a sondar",
            "{decisao:?}"
        );
    }

    #[test]
    fn disjuntor_aberto_serve_degradado_sem_sondar() {
        let relogio = RelogioManual::fixo(InstanteUtc::epoch());
        let fonte = FonteResiliente::nova(
            "a.ntp.br",
            espera_de_teste(),
            disjuntor_padrao(),
            calibracao_de(amostra_valida(), &relogio),
        );
        for _ in 0..3 {
            fonte.disjuntor().registrar_falha(InstanteUtc::epoch());
        }
        let decisao = fonte.decidir(InstanteUtc::epoch(), Duration::from_millis(250));
        exigir!(
            decisao.evita_rede(),
            "fonte fora do ar nao deve gerar trafego de sondagem",
            "{decisao:?}"
        );
    }

    /// Amostra valida: desvio de +5 ms, atraso de 18 ms.
    ///
    /// Nao usa `crate::fixture::amostra_de_teste` de proposito: aqui a
    /// dobrada e' construida passo a passo, para que o teste dependa da
    /// geometria T1..T4 e nao de uma abstração. Se a dobrada de `fixture`
    /// mudar, este teste continua provando sobre a geometria real.
    fn amostra_valida() -> AmostraDeTempo {
        let politica = politica_de_teste();
        let leituras = crate::fixture::de_ou_panica(
            LeiturasBrutas::de_quatro(
                InstanteUtc::epoch(),
                InstanteUtc::de_milissegundos(15),
                InstanteUtc::de_milissegundos(17),
                InstanteUtc::de_milissegundos(22),
            ),
            "leituras de teste (T1=0 T2=15 T3=17 T4=22ms)",
        );
        crate::fixture::de_ou_panica(
            AmostraDeTempo::de_bruta(
                leituras,
                Estrato::Primario,
                None,
                InstanteUtc::epoch(),
                &politica,
            ),
            "amostra de teste (desvio +5ms, atraso 20ms)",
        )
    }
}