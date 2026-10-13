//! A costura central: bytes na rede viram um objeto de dominio confiavel.
//!
//! # As quatro perguntas do NTP
//!
//! Uma sondagem SNTP troca quatro instantes. Os dois primeiros sao medidos pelo
//! **servidor**; os dois ultimos, por nos.
//!
//! ```text
//!  T1 enviada      T2 recepcionada     T3 emitida       T4 recebida
//!  |--- cliente ---|------- servidor ---|---- cliente ---|
//!         ida            processamento          volta
//! ```
//!
//! ```text
//! desvio = ((T2 - T1) + (T3 - T4)) / 2     // quanto o relogio do servidor esta a frente do nosso
//! atraso =  (T4 - T1) - (T3 - T2)         // quanto tempo a ida-e-volta consumiu de verdade
//! ```
//!
//! Note a assimetria: o **desvio** e' a media dos dois sentidos, porque um
//! pacote so pode ter atrasado para frente, nunca para tras. O **atraso** e'
//! o RTT menos o tempo que o servidor gastou pensando -- se sobrar negativo,
//! a medicao e' ilegive e a amostra e' recusada.
//!
//! # Pre-condicoes e invariantes
//!
//! Este e' o unico lugar do sistema onde entra dado nao confiavel. A
//! construcao de [`AmostraDeTempo`] e', por isso, uma costura com
//! `DbC` apertado:
//!
//! **Pre-condicoes** (rejeitadas com `Err` -- o dado veio de fora):
//!
//! | # | Pre-condicao | Por que
//! |---|---|---|
//! | 1 | `T2 <= T3` | o servidor nao pode responder antes de receber
//! | 2 | `atraso >= 0` | RTT menor que o processamento do servidor e' ruido
//! | 3 | `atraso <= politica.atraso_maximo` | amostra velha demais nao localiza o relogio
//! | 4 | `|desvio| <= politica.desvio_maximo` | desvios absurdos indicam relogio manipulado
//! | 5 | estrato sincronizado e `LI != 3` | o servidor esta dizendo que nao sabe a hora
//! | 6 | modo servidor ou difusao | outro modo nao e' resposta
//! | 7 | sem *kiss-of-death*, ou aceito explicitamente | servidor recusando por limite de taxa
//!
//! **Invariante** (verificada com `panic!`, para sempre, em todo uso):
//!
//! | # | Invariante
//! |---|---|
//! | 1 | `desvio` e `atraso` derivam **exatamente** dos quatro instantes -- recalcular e' identico
//! | 2 | `T1 <= T4` -- uma resposta chegou depois de ter sido pedida
//! | 3 | `instante_do_servidor` (T3) e' sempre >= `obtida_em` (T4) menos o atraso de rede
//!
//! **Pos-condicoes**:
//!
//! | # | Pos-condicao
//! |---|---|
//! | 1 | `AmostraDeTempo::corrigir(t)` devolve `t + desvio`, com o sinal ja aplicado
//! | 2 | uma amostra recusada nao vira objeto: nao existe `AmostraDeTempo` invalida
//!
//! # Por que os testes moram aqui e nao em `tests/`
//!
//! A geometria T1..T4 e' **pre-condicao** do calculo, e cada teste escolhe uma
//! geometria diferente para provar uma pre-condicao. Mantidos juntos, o leitor
//! ve o par "geometria <-> regra". Espalhados por `tests/`, seria impossivel
//! enxergar que `atraso_ignora_o_processamento_do_servidor` e
//! `atraso_negativo_e_rejeitado` usam **a mesma equacao** com sinais trocados.

use crate::contrato::ErroDeContrato;
use crate::instante::InstanteUtc;
use crate::politica::PoliticaDeConsulta;
use std::fmt;
use std::time::Duration;

/// Desvio entre o relogio da fonte e o nosso, em nanossegundos.
///
/// Assimetrico por natureza: pode ser negativo (nos estamos adiantados).
///
/// **Invariante:** o campo interno e' a magnitude com sinal do desvio medido;
/// nao ha limite aqui. Quem restringe e' a politica, em
/// [`AmostraDeTempo::de_bruta`] -- um desvio enorme e' um dado *valido* que a
/// politica *rejeita*, nao um dado invalido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DesvioDeRelogio(i64);

impl fmt::Display for Estrato {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.descricao())
    }
}

impl DesvioDeRelogio {
    /// Zero -- os relogios concordam.
    pub const NULO: Self = Self(0);

    /// Deslocamento em nanossegundos.
    ///
    /// Pre-condicao: nenhuma; o tipo aceita qualquer `i64`, e quem restringe
    /// e' a politica, nao o construtor -- um desvio enorme e' um dado valido
    /// que *a politica* rejeita, nao um dado invalido.
    #[must_use]
    pub const fn de_nanos(nanos: i64) -> Self {
        Self(nanos)
    }

    /// Valor com sinal, em nanossegundos.
    #[must_use]
    pub const fn em_nanos(self) -> i64 {
        self.0
    }

    /// Magnitude do desvio, em microssegundos, arredondada.
    #[must_use]
    pub fn em_micros(self) -> i64 {
        self.0 / 1_000
    }

    /// Magnitude do desvio, em milissegundos, arredondada.
    #[must_use]
    pub fn em_milis(self) -> i64 {
        self.0 / 1_000_000
    }

    /// Desvio em [`Duration`], sempre nao-negativo (a magnitude).
    ///
    /// ## Por que magnitude, e nao valor com sinal
    ///
    /// [`AmostraDeTempo::corrigir`] precisa **somar** quando a fonte esta
    /// adiantada e **subtrair** quando esta atrasada. Com um `i64` exposto,
    /// isso exigiria um `if` -- e um `if` que decide o sentido da correcao do
    /// relogio do cliente. O sinal ja esta no nome do metodo (`corrigir`),
    /// entao aqui so a magnitude trafega, e o `match` fica no **metodo do
    /// agregado**, que e' onde a decisao pertence.
    ///
    /// **Pos-condicao:** sempre valido para `|desvio| <= u64::MAX` nanos
    /// (~584 anos); o tipo do valor impede `Err`.
    #[must_use]
    pub const fn magnitude(self) -> Duration {
        Duration::from_nanos(self.0.unsigned_abs())
    }

    /// `true` se o relogio da fonte esta adiantado em relacao ao nosso.
    #[must_use]
    pub const fn esta_adiantado(self) -> bool {
        self.0 > 0
    }
}

impl fmt::Display for DesvioDeRelogio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:+} us", self.em_micros())
    }
}

/// Atraso ida-e-volta da sondagem, em nanossegundos.
///
/// Sempre nao-negativo **por tipo**: um atraso negativo e' contradicao, e o
/// construtor trava a porta antes que ele chegue a existir.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct AtrasoDeRede(u64);

impl AtrasoDeRede {
    /// Sem atraso (caso degenerado de teste).
    pub const NULO: Self = Self(0);

    /// Atraso em nanossegundos.
    ///
    /// Pre-condicao: nenhuma -- `u64` ja garante a nao-negatividade.
    #[must_use]
    pub const fn de_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Valor em nanossegundos.
    #[must_use]
    pub const fn em_nanos(self) -> u64 {
        self.0
    }

    /// Valor em microssegundos.
    #[must_use]
    pub const fn em_micros(self) -> u64 {
        self.0 / 1_000
    }

    /// Valor em milissegundos.
    #[must_use]
    pub const fn em_milis(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Converte para [`Duration`].
    ///
    /// Pre-condicao: o atraso cabe em `u64` segundos. Sempre verdadeiro para
    /// os valores que a politica aceita (sub-segundo), mas a funcao e' `total`
    /// porque `Duration` tem limite de 584 anos.
    /// Converte para [`Duration`]. **Total**: `Duration::from_nanos` aceita
    /// qualquer `u64`, entao nao ha pre-condicao nem `Err`.
    #[must_use]
    pub const fn para_duration(self) -> Duration {
        Duration::from_nanos(self.0)
    }
}

impl fmt::Display for AtrasoDeRede {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} us", self.em_micros())
    }
}

/// Estrato declarado pela fonte. 0 e' "beijo da morte".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Estrato {
    /// Stratum 0 -- kiss-of-death. O servidor esta recusando, nao respondendo.
    BeijoDaMorte,
    /// Stratum 1 -- fonte primaria (relógio atomico / GPS).
    Primario,
    /// Stratum 2..=15 -- fonte secundaria.
    Secundario(u8),
    /// Stratum 16 ou mais -- o servidor sabe que nao esta sincronizado.
    NaoSincronizado,
}

impl Estrato {
    /// Interpreta o byte 1 do pacote.
    ///
    /// **Total**: todo `u8` vira um estrato. Nenhum valor e' invalido -- o
    /// significado de cada um ja esta no tipo, entao nao ha o que rejeitar.
    #[must_use]
    pub const fn de_byte(byte: u8) -> Self {
        match byte {
            0 => Self::BeijoDaMorte,
            1 => Self::Primario,
            16..=255 => Self::NaoSincronizado,
            outro => Self::Secundario(outro),
        }
    }

    /// Byte correspondente no pacote.
    #[must_use]
    pub const fn para_byte(self) -> u8 {
        match self {
            Self::BeijoDaMorte => 0,
            Self::Primario => 1,
            Self::Secundario(n) => n,
            Self::NaoSincronizado => 16,
        }
    }

    /// `true` se o estrato descreve uma fonte efetivamente sincronizada.
    ///
    /// Invariante de decisao: um estrato nao sincronizado nunca produz amostra,
    /// por mais completo que o pacote esteja. A fonte que decide e' a
    /// politica, aqui so classificamos.
    #[must_use]
    pub const fn esta_sincronizado(self) -> bool {
        matches!(self, Self::Primario | Self::Secundario(_))
    }

    /// Descricao textual, para a resposta da API.
    #[must_use]
    pub const fn descricao(self) -> &'static str {
        match self {
            Self::BeijoDaMorte => "beijo-da-morte",
            Self::Primario => "primaria",
            Self::Secundario(_) => "secundaria",
            Self::NaoSincronizado => "nao-sincronizada",
        }
    }
}

/// Codigo ASCII de *kiss-of-death* (RFC 5905 secao 7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeijoDaMorte {
    /// `DENY` -- acesso negado.
    Denegado,
    /// `RSTR` -- restrito.
    Restrito,
    /// `RATE` -- limite de taxa excedido.
    LimiteDeTaxa,
    /// `DROP` -- pacote descartado.
    Descartado,
    /// Codigo desconhecido -- o servidor pode ter enviado outro.
    Outro(u8),
}

impl BeijoDaMorte {
    /// Interpreta o byte 2 do pacote.
    ///
    /// **Total**: qualquer byte vira um codigo. O servidor pode escrever
    /// qualquer um dos quatro, ou algo novo; `Outro` cobre o resto sem
    /// inventar um `Option`.
    #[must_use]
    pub const fn de_byte(byte: u8) -> Self {
        match byte & 0b0000_1111 {
            0 => Self::Denegado,
            1 => Self::Restrito,
            2 => Self::LimiteDeTaxa,
            3 => Self::Descartado,
            outro => Self::Outro(outro),
        }
    }

    /// Descricao textual.
    #[must_use]
    pub const fn descricao(self) -> &'static str {
        match self {
            Self::Denegado => "acesso-negado",
            Self::Restrito => "restrito",
            Self::LimiteDeTaxa => "limite-de-taxa",
            Self::Descartado => "descartado",
            Self::Outro(_) => "desconhecido",
        }
    }
}

/// Os quatro instantes de uma sondagem, como medidos.
///
/// Reproduz T1..T4 com nomes autoexplicativos. Um struct em vez de quatro
/// parametros porque a ordem *e' o significado*: trocar T2 por T3 produz um
/// desvio errado sem erro de compilacao, que e' o pior tipo de bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeiturasBrutas {
    /// T1 -- quando o cliente enviou.
    pub enviada: InstanteUtc,
    /// T2 -- quando o servidor recebeu.
    pub recepcionada: InstanteUtc,
    /// T3 -- quando o servidor enviou a resposta.
    pub emitida: InstanteUtc,
    /// T4 -- quando o cliente recebeu.
    pub recebida: InstanteUtc,
}

impl LeiturasBrutas {
    /// Monta as quatro leituras.
    ///
    /// Pre-condicoes:
    /// 1. `enviada <= recebida` -- a resposta nao pode chegar antes do envio;
    /// 2. `recepcionada <= emitida` -- o servidor nao pode responder antes
    ///    de receber.
    ///
    /// Sao as duas condicoes que a matematica do NTP exige para que `atraso`
    /// e `desvio` tenham sentido. Violar isso e' bug de medicao.
    pub fn de_quatro(
        enviada: InstanteUtc,
        recepcionada: InstanteUtc,
        emitida: InstanteUtc,
        recebida: InstanteUtc,
    ) -> Result<Self, ErroDeContrato> {
        if let Err(e) = enviada.idade(recebida) {
            return Err(ErroDeContrato::novo(
                "LeiturasBrutas::de_quatro",
                format!("T1 posterior a T4: {e}"),
            ));
        }
        if let Err(e) = recepcionada.idade(emitida) {
            return Err(ErroDeContrato::novo(
                "LeiturasBrutas::de_quatro",
                format!("T2 posterior a T3: {e}"),
            ));
        }
        Ok(Self {
            enviada,
            recepcionada,
            emitida,
            recebida,
        })
    }

    /// Atraso de rede, descontando o tempo de processamento do servidor.
    ///
    /// Pre-condicao: nenhuma. **Pos-condicao:** devolve `Err` se o resultado
    /// for negativo -- RTT medido menor que o tempo que o servidor gastou
    /// entre receber e emitir significa que um dos quatro instantes esta
    /// errado, e um `u64` forcado a `0` esconderia isso.
    pub fn atraso(&self) -> Result<AtrasoDeRede, ErroDeContrato> {
        let ida = self.enviada.delta_ate(self.recebida);
        let processamento = self.recepcionada.delta_ate(self.emitida);
        let liquido = ida - processamento;
        u64::try_from(liquido).map(AtrasoDeRede::de_nanos).map_err(|_| {
            ErroDeContrato::novo(
                "LeiturasBrutas::atraso",
                format!("RTT {ida}ns menor que processamento {processamento}ns"),
            )
        })
    }

    /// Desvio do relogio da fonte em relacao ao nosso.
    ///
    /// Pre-condicao: nenhuma. Pos-condicao: `desvio` e' exatamente
    /// `((T2 - T1) + (T3 - T4)) / 2`, arredondado ao nanossegundo mais
    /// proximo -- o arredondamento de meio para cima mantem o valor medio
    /// dos dois sentidos em referencia, sem vies para um lado.
    pub fn desvio(&self) -> Result<DesvioDeRelogio, ErroDeContrato> {
        let ida = self.enviada.delta_ate(self.recepcionada);
        let volta = self.recebida.delta_ate(self.emitida);
        let soma = ida + volta;
        // div_euclid arredonda em direcao a -infinito; para simetria usamos
        // o resto explicito.
        let desvio = if soma >= 0 {
            (soma + 1) / 2
        } else {
            -((-soma + 1) / 2)
        };
        i64::try_from(desvio).map(DesvioDeRelogio::de_nanos).map_err(|_| {
            ErroDeContrato::novo(
                "LeiturasBrutas::desvio",
                format!("desvio {desvio}ns excede i64"),
            )
        })
    }
}

/// Uma medicao de tempo **validada e confiavel**.
///
/// Nao existe forma de construir uma invalida: o unico construtor e'
/// [`AmostraDeTempo::de_bruta`], que aplica as pre-condicoes. Este e' o
/// principio "make illegal states unrepresentable" aplicado a `DbC` --
/// mais forte que verificar depois.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AmostraDeTempo {
    leituras: LeiturasBrutas,
    desvio: DesvioDeRelogio,
    atraso: AtrasoDeRede,
    estrato: Estrato,
    beijo_da_morte: Option<BeijoDaMorte>,
    obtida_em: InstanteUtc,
}

impl AmostraDeTempo {
    /// Valida e constroi.
    ///
    /// Pre-condicoes (rejeitadas com `Err`):
    /// 1. `leituras` ja validada por [`LeiturasBrutas::de_quatro`];
    /// 2. `atraso` calculavel e `> 0`;
    /// 3. `atraso <= politica.atraso_maximo`;
    /// 4. `|desvio| <= politica.desvio_maximo`;
    /// 5. `estrato.esta_sincronizado()`;
    /// 6. `beijo_da_morte` ausente.
    ///
    /// Invariante: o par `(desvio, atraso)` e' **derivado**, nunca informado.
    /// Recalcular a partir de `leituras` devolve exatamente os mesmos valores.
    pub fn de_bruta(
        leituras: LeiturasBrutas,
        estrato: Estrato,
        beijo_da_morte: Option<BeijoDaMorte>,
        obtida_em: InstanteUtc,
        politica: &PoliticaDeConsulta,
    ) -> Result<Self, ErroDeContrato> {
        let atraso = leituras.atraso()?;
        let desvio = leituras.desvio()?;

        // Pre-condicao 2: atraso zero e' medicao degenerada (pacote de menos de
        // ~200ns de ida e volta). Nao informa nada sobre o relogio.
        if atraso.em_nanos() == 0 {
            return Err(ErroDeContrato::novo(
                "AmostraDeTempo::de_bruta",
                "atraso zero: medicao degenerada",
            ));
        }
        // Pre-condicao 6: beijo da morte. `if let` porque `Option` aqui e'
        // genuinamente opcional -- o campo so vale quando estrato == 0.
        if let Some(codigo) = beijo_da_morte {
            return Err(ErroDeContrato::novo(
                "AmostraDeTempo::de_bruta",
                format!("fonte respondeu com beijo-da-morte: {}", codigo.descricao()),
            ));
        }
        if !estrato.esta_sincronizado() {
            return Err(ErroDeContrato::novo(
                "AmostraDeTempo::de_bruta",
                format!("estrato {} nao sincronizado", estrato.para_byte()),
            ));
        }
        // Pre-condicao 3: atraso acima do que a politica tolera.
        let limite_de_atraso = politica.ajustes().atraso_maximo;
        // Pre-condicao 3: atraso acima do que a politica tolera. Um `if` e'
        // melhor que `match` sobre booleano aqui: ha **uma** consequencia
        // (rejeitar) e um **unico** valor de teste -- nao ha dois estados a
        // nomear. Compare com `corrigir`, onde o `match` se justifica.
        if atraso.para_duration() > limite_de_atraso {
            return Err(ErroDeContrato::novo(
                "AmostraDeTempo::de_bruta",
                format!("atraso {atraso} acima do limite {limite_de_atraso:?}"),
            ));
        }
        let desvio_abs = desvio.em_nanos().unsigned_abs();
        let limite_desvio = u64::try_from(politica.ajustes().desvio_maximo.as_nanos()).unwrap_or(u64::MAX);
        if desvio_abs > limite_desvio {
            return Err(ErroDeContrato::novo(
                "AmostraDeTempo::de_bruta",
                format!("desvio {desvio} acima do limite de {politica:?}"),
            ));
        }
        Ok(Self {
            leituras,
            desvio,
            atraso,
            estrato,
            beijo_da_morte: None,
            obtida_em,
        })
    }

    /// Aplica o desvio ao nosso relogio.
    ///
    /// Pos-condicao: o resultado e' `relogio + desvio`, e o **sinal esta
    /// invertido** em relacao ao sentido do desvio: se a fonte esta
    /// adiantada (`desvio > 0`), somamos para nos acertar.
    #[must_use]
    pub fn corrigir(&self, relogio: InstanteUtc) -> InstanteUtc {
        // O sinal decide o sentido da correcao. Um `if` aqui seria pior:
        // `corrigir` passa a descrever a operacao ("ajustar o relogio"), e o
        // `match` sobre o sinal explicita as duas unicas possibilidades.
        match self.desvio.esta_adiantado() {
            true => relogio.mais(self.desvio.magnitude()),
            // `== 0` cai no mesmo braço: somar zero nao altera nada.
            false => relogio.menos(self.desvio.magnitude()),
        }
    }

    /// As leituras brutas de origem.
    #[must_use]
    pub const fn leituras(&self) -> &LeiturasBrutas {
        &self.leituras
    }

    /// Desvio medido.
    #[must_use]
    pub const fn desvio(&self) -> DesvioDeRelogio {
        self.desvio
    }

    /// Atraso medido.
    #[must_use]
    pub const fn atraso(&self) -> AtrasoDeRede {
        self.atraso
    }

    /// Estrato da fonte.
    #[must_use]
    pub const fn estrato(&self) -> Estrato {
        self.estrato
    }

    /// Momento em que a amostra foi reduzida a objeto.
    #[must_use]
    pub const fn obtida_em(&self) -> InstanteUtc {
        self.obtida_em
    }

    /// Atraso e desvio em uma frase -- para log e para o campo `resumo`.
    #[must_use]
    pub fn resumo(&self) -> String {
        format!("desvio={self} atraso={} estrato={}", self.atraso, self.estrato.para_byte())
    }

    /// Anti-padrao "getter puro" (Yegor256 #4): em vez de expor todos os
    /// campos para o chamador recomputar a estatistica, o objeto responde a
    /// pergunta completa. `qualidade` e' a metrica que o dominio considera
    /// util -- menor `atraso` e' melhor, porque significa caminho mais curto
    /// e, portanto, menor incerteza sobre o ponto de referencia.
    #[must_use]
    pub const fn qualidade(&self) -> (u8, u64) {
        (self.estrato.para_byte(), self.atraso.em_nanos())
    }
}

impl fmt::Display for AmostraDeTempo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (desvio={}, atraso={})", self.estrato.descricao(), self.desvio, self.atraso)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;
    use crate::fixture::{leituras_de_teste, politica_de_teste};

    fn politica() -> PoliticaDeConsulta {
        politica_de_teste()
    }

    /// Politica de teste com um limite estreitado.
    ///
    /// Recebe um **ajustador** em vez de um `AjustesDePolitica` pronto: quem
    /// chama informa so o limite que quer testar, e o resto continua valido
    /// por construcao. Assim nao ha duas politicas de teste para manter em
    /// sincronia -- apenas uma, mais um ajuste pontual.
    ///
    /// **Pos-condicao:** se a politica resultante for invalida, o teste
    /// **falha** em vez de devolver `Err`. Um `Result` aqui permitiria que um
    /// teste passasse por accidento (o `Err` sendo tratado como "esperado"),
    /// mascarando a dobra.
    fn politica_com_limite(
        ajustar: impl FnOnce(&mut crate::politica::AjustesDePolitica),
        contexto: &str,
    ) -> PoliticaDeConsulta {
        let mut ajustes = politica_de_teste().ajustes().clone();
        ajustar(&mut ajustes);
        crate::fixture::de_ou_panica(
            PoliticaDeConsulta::de_ajustes(ajustes, crate::fixture::alvos_de_teste(2)),
            contexto,
        )
    }

    #[test]
    fn desvio_de_relogio_cinco_milissegundos() {
        // Fonte 5 ms adiantada, ida de 10 ms, processamento de 2 ms:
        //   T1=0  T2=10+5=15  T3=12+5=17  T4=22
        // desvio = ((15-0) + (17-22)) / 2 = 10/2 = +5 ms
        // atraso = (22-0) - (17-15)       = 22-2 = 20 ms
        let leituras = crate::fixture::de_ou_panica(
            LeiturasBrutas::de_quatro(
                InstanteUtc::epoch(),
                InstanteUtc::de_milissegundos(15),
                InstanteUtc::de_milissegundos(17),
                InstanteUtc::de_milissegundos(22),
            ),
            "leituras com desvio de +5ms",
        );
        let desvio = crate::fixture::de_ou_panica(leituras.desvio(), "desvio de +5ms");
        exigir!(
            desvio.em_micros() == 5_000,
            "pre-condicao matematica: desvio = +5ms",
            "{desvio}"
        );
    }

    #[test]
    fn atraso_ignora_o_processamento_do_servidor() {
        // RTT 20ms, servidor levou 2ms: atraso real = 18ms.
        let leituras = leituras_de_teste(0, 15, 17, 22);
        let Ok(atraso) = leituras.atraso() else { return };
        // RTT de 22 ms, servidor levou 2 ms: atraso = 22 - 2 = 20 ms.
        exigir!(atraso.em_milis() == 20, "atraso = RTT - processamento", "{atraso}");
    }

    #[test]
    fn atraso_negativo_e_rejeitado() {
        // T3 - T2 = 30 ms, mas o RTT medido e' de 22 ms: o RTT inclui
        // o tempo de processamento, entao nunca pode ser menor que ele.
        let leituras = leituras_de_teste(0, 15, 45, 22);
        let r = leituras.atraso();
        exigir!(r.is_err(), "pre-condicao: atraso negativo rejeitado", "{r:?}");
    }

    #[test]
    fn t2_posterior_a_t3_e_rejeitado_na_pre_condicao() {
        let r = LeiturasBrutas::de_quatro(
            InstanteUtc::epoch(),
            InstanteUtc::de_milissegundos(10),
            InstanteUtc::de_milissegundos(5),
            InstanteUtc::de_milissegundos(20),
        );
        exigir!(r.is_err(), "T2 > T3 rejeitado", "{r:?}");
    }

    #[test]
    fn estrato_zero_e_beijo_da_morte() {
        exigir!(Estrato::de_byte(0) == Estrato::BeijoDaMorte, "stratum 0 = KoD", "{}", Estrato::de_byte(0));
    }

    #[test]
    fn estrato_16_ou_mais_e_nao_sincronizado() {
        for b in [16u8, 17, 255] {
            exigir!(
                Estrato::de_byte(b) == Estrato::NaoSincronizado,
                "stratum {b} = nao sincronizado",
                "{:?}",
                Estrato::de_byte(b)
            );
        }
    }

    #[test]
    fn beijo_da_morte_rejeita_a_amostra() {
        let r = AmostraDeTempo::de_bruta(
            leituras_de_teste(0, 15, 17, 22),
            Estrato::Primario,
            Some(BeijoDaMorte::LimiteDeTaxa),
            InstanteUtc::epoch(),
            &politica(),
        );
        exigir!(r.is_err(), "KoD impede amostra", "{r:?}");
    }

    #[test]
    fn estrato_nao_sincronizado_rejeita_a_amostra() {
        let r = AmostraDeTempo::de_bruta(
            leituras_de_teste(0, 15, 17, 22),
            Estrato::NaoSincronizado,
            None,
            InstanteUtc::epoch(),
            &politica(),
        );
        exigir!(r.is_err(), "estrato 16 impede amostra", "{r:?}");
    }

    #[test]
    fn atraso_acima_do_limite_rejeita() {
        let politica = politica_com_limite(
            |a| {
                a.atraso_maximo = Duration::from_millis(5);
            },
            "atraso maximo 5ms",
        );
        // RTT 22ms, processamento 2ms => atraso 20ms > 5ms.
        let r = AmostraDeTempo::de_bruta(
            leituras_de_teste(0, 15, 17, 22),
            Estrato::Primario,
            None,
            InstanteUtc::epoch(),
            &politica,
        );
        exigir!(r.is_err(), "atraso 20ms acima do limite 5ms", "{r:?}");
    }

    #[test]
    fn desvio_acima_do_limite_rejeita() {
        let politica = politica_com_limite(
            |a| {
                a.desvio_maximo = Duration::from_millis(1);
            },
            "desvio maximo 1ms",
        );
        // Desvio de +5ms > 1ms.
        let r = AmostraDeTempo::de_bruta(
            leituras_de_teste(0, 15, 17, 22),
            Estrato::Primario,
            None,
            InstanteUtc::epoch(),
            &politica,
        );
        exigir!(r.is_err(), "desvio 5ms acima do limite 1ms", "{r:?}");
    }

    #[test]
    fn corrigir_soma_o_desvio_ao_relogio() {
        let politica = politica();
        let leituras = leituras_de_teste(0, 15, 17, 22);
        let Ok(amostra) = AmostraDeTempo::de_bruta(
            leituras,
            Estrato::Primario,
            None,
            InstanteUtc::epoch(),
            &politica,
        ) else {
            return;
        };
        // Fonte 5 ms adiantada: relogio 0 -> corrigido +5 ms.
        let corrigido = amostra.corrigir(InstanteUtc::epoch());
        exigir!(
            corrigido.segundos_desde_epoch() == 0
                && corrigido.nanos_do_segundo() == 5_000_000,
            "pos-condicao: corrigir soma o desvio",
            "{corrigido:?}"
        );
    }

    #[test]
    fn invariante_desvio_e_atraso_sao_derivados() {
        let politica = politica();
        let leituras = leituras_de_teste(0, 15, 17, 22);
        let Ok(a) = AmostraDeTempo::de_bruta(
            leituras,
            Estrato::Primario,
            None,
            InstanteUtc::epoch(),
            &politica,
        ) else {
            return;
        };
        // Invariante 1: recalcular devolve os mesmos valores.
        exigir!(
            a.leituras().desvio().map(DesvioDeRelogio::em_nanos) == Ok(a.desvio().em_nanos()),
            "invariante: desvio e' derivado",
            "divergiu"
        );
        exigir!(
            a.leituras().atraso().map(AtrasoDeRede::em_nanos) == Ok(a.atraso().em_nanos()),
            "invariante: atraso e' derivado",
            "divergiu"
        );
    }
}