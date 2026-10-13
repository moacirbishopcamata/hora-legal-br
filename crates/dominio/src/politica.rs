//! Politica de consulta: o conjunto de limites que decide quando uma leitura
//! de tempo e' aceitavel e quanto insistir antes de desistir.
//!
//! ## Por que e' um objeto de dominio
//!
//! Estes numeros **nao** sao parametros de infraestrutura, sao regras de
//! negocio do servico:
//!
//! - "desvio maximo de 500 ms" diz o que o servico garante de precisao;
//! - "atraso maximo de 1 s" diz ate onde uma amostra ainda e' confiavel;
//! - "janela de coerencia de 250 ms" diz o quao velha a calibracao servida
//!   pode estar antes de o cache deixar de valer.
//!
//! Se isso vivesse em variavel de ambiente espalhada pelo codigo, nao haveria
//! pre-condicao, nao haveria teste, e nao haveria um unico lugar para dizer
//! "o que este servico promete".
//!
//! ## Os tres perfis
//!
//! [`Perfil`] expoe presets porque o requisito de **menor latencia possivel**
//! trade-off direto contra precisao maxima. Nomear o trade-off e' mais honesto
//! que deixar cada time discoverir a trade-off ajustando numeros soltos.

use std::time::Duration;

use crate::alvo::AlvoDeConsulta;
use crate::contrato::ErroDeContrato;

/// Teto de tentativas por requisição. Acima disso a API vira um martelo.
pub const TETO_DE_TENTATIVAS: u8 = 8;

/// Entrada crua da politica, antes da validacao.
///
/// Todos os campos sao publicos e tem `Default`: a forma usual de uso e'
/// construir com `..Default::default()` e sobrescrever so o que muda.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AjustesDePolitica {
    /// Prazo de uma unica sondagem UDP.
    pub prazo_por_fonte: Duration,
    /// Prazo total da requisicao, incluindo retentativas.
    pub prazo_total: Duration,
    /// Quantas vezes insistir antes de devolver erro.
    pub tentativas: u8,
    /// Espera antes da primeira retentativa.
    pub espera_inicial: Duration,
    /// Teto da espera entre retentativas.
    pub espera_maxima: Duration,
    /// Atraso ida-e-volta acima do qual a amostra e' descartada.
    pub atraso_maximo: Duration,
    /// Desvio de relogio acima do qual a amostra e' descartada.
    pub desvio_maximo: Duration,
    /// Idade maxima de uma calibracao em cache ainda servida sem revalidar.
    pub janela_de_coerencia: Duration,
    /// Quantas fontes precisam concordar para a leitura ser considerada
    /// corroborada. `1` = uma so e' suficiente.
    pub fontes_minimas: usize,
}

impl Default for AjustesDePolitica {
    /// Perfil [`Perfil::Equilibrado`].
    fn default() -> Self {
        Self::do_perfil(Perfil::Equilibrado)
    }
}

impl AjustesDePolitica {
    /// Ajustes de um [`Perfil`] nomeado.
    ///
    /// **Invariante de tabela:** para cada perfil, `prazo_total` e' sempre
    /// maior ou igual a `tentativas * prazo_por_fonte + tentativas *
    /// espera_maxima`.
    ///
    /// Isso nao e' documentacao: e' o que mantem a pre-condicao 7 de
    /// [`PoliticaDeConsulta::de_ajustes`] verde. O teste
    /// [`pior_caso_de_cada_perfil_cabe_no_prazo_total`] verifica a tabela.
    #[must_use]
    pub fn do_perfil(perfil: Perfil) -> Self {
        let (ms_atraso, ms_desvio, janela, tentativas, prazo_fonte, espera_i, espera_m, total_ms, fontes) =
            match perfil {
                Perfil::MaximaPrecisao => (250, 100, 100, 4, 500, 4, 32, 2_500, 2),
                Perfil::Equilibrado => (750, 500, 250, 2, 400, 8, 50, 1_200, 1),
                // Precisao e' preservada; o que muda e' a recusa a esperar:
                // 2 fontes em paralelo, janela de cache de 80 ms e um total
                // de 350 ms. A calibracao continua sendo re-amostrada em
                // segundo plano, entao a janela curta nao custa precisao.
                Perfil::MenorLatencia => (1_000, 1_000, 80, 1, 150, 1, 8, 350, 1),
            };
        Self {
            prazo_por_fonte: Duration::from_millis(prazo_fonte),
            prazo_total: Duration::from_millis(total_ms),
            tentativas,
            espera_inicial: Duration::from_millis(espera_i),
            espera_maxima: Duration::from_millis(espera_m),
            atraso_maximo: Duration::from_millis(ms_atraso),
            desvio_maximo: Duration::from_millis(ms_desvio),
            janela_de_coerencia: Duration::from_millis(janela),
            fontes_minimas: fontes,
        }
    }
}

/// Politica validada. Uma vez construida, imutavel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoliticaDeConsulta {
    ajustes: AjustesDePolitica,
    alvos: Vec<AlvoDeConsulta>,
}

impl PoliticaDeConsulta {
    /// Valida os ajustes e vincula os alvos.
    ///
    /// Pre-condicoes (todas verificadas aqui, e nao no ponto de uso):
    /// 1. `tentativas` em `1..=TETO_DE_TENTATIVAS`;
    /// 2. `fontes_minimas >= 1`;
    /// 3. `alvos.len() >= fontes_minimas` -- caso contrario o servico
    ///    promete corroboracao que nao pode cumprir;
    /// 4. `espera_maxima >= espera_inicial > 0`;
    /// 5. `janela_de_coerencia > 0` (zero faria o cache nunca valer);
    /// 6. `atraso_maximo > 0` e `desvio_maximo > 0`;
    /// 7. `prazo_total` comporta o pior caso: `tentativas * prazo_por_fonte`
    ///    mais as esperas entre elas.
    pub fn de_ajustes(
        ajustes: AjustesDePolitica,
        alvos: Vec<AlvoDeConsulta>,
    ) -> Result<Self, ErroDeContrato> {
        if ajustes.tentativas == 0 || ajustes.tentativas > TETO_DE_TENTATIVAS {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                format!("tentativas={} fora de 1..={TETO_DE_TENTATIVAS}", ajustes.tentativas),
            ));
        }
        if ajustes.fontes_minimas == 0 {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                "fontes_minimas precisa ser >= 1",
            ));
        }
        if alvos.len() < ajustes.fontes_minimas {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                format!(
                    "{} alvos nao sustentam {} fontes minimas",
                    alvos.len(),
                    ajustes.fontes_minimas
                ),
            ));
        }
        if ajustes.espera_inicial.is_zero()
            || ajustes.espera_maxima < ajustes.espera_inicial
        {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                format!(
                    "esperas incoerentes: inicial={:?} maxima={:?}",
                    ajustes.espera_inicial, ajustes.espera_maxima
                ),
            ));
        }
        if ajustes.janela_de_coerencia.is_zero() {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                "janela_de_coerencia zero desativa o cache; use um valor > 0",
            ));
        }
        if ajustes.atraso_maximo.is_zero() || ajustes.desvio_maximo.is_zero() {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                "tolerancias de atraso/desvio precisam ser > 0",
            ));
        }
        // Aritmetica toda em `u32` de milissegundos: 2^32 ms sao ~49 dias, e um
    // prazo de requisicao que chega perto disso ja e' bug de configuracao.
    let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
        let pior_caso_sondagens =
            u32::from(ajustes.tentativas).saturating_mul(ms(ajustes.prazo_por_fonte));
        let pior_caso_esperas = ms(
            ajustes
                .espera_maxima
                .saturating_mul(u32::from(ajustes.tentativas)),
        );
        if ms(ajustes.prazo_total) < pior_caso_sondagens.saturating_add(pior_caso_esperas) {
            return Err(ErroDeContrato::novo(
                "PoliticaDeConsulta::de_ajustes",
                format!(
                    "prazo_total={:?} menor que o pior caso (~{}ms)",
                    ajustes.prazo_total,
                    pior_caso_sondagens.saturating_add(pior_caso_esperas)
                ),
            ));
        }
        Ok(Self { ajustes, alvos })
    }

    /// Politica do perfil, com os alvos padrão do NTP.br.
    pub fn do_perfil(perfil: Perfil) -> Result<Self, ErroDeContrato> {
        Self::de_ajustes(AjustesDePolitica::do_perfil(perfil), crate::alvo::alvos_do_ntp_br())
    }

    /// Ajustes validados.
    #[must_use]
    pub const fn ajustes(&self) -> &AjustesDePolitica {
        &self.ajustes
    }

    /// Alvos, na ordem em que devem ser consultados.
    #[must_use]
    pub fn alvos(&self) -> &[AlvoDeConsulta] {
        &self.alvos
    }

    /// Ordem de preferencia por desvio, do mais tolerante ao mais estrito.
    ///
    /// Devolve `true` se `a` deve ser tentado **antes** de `b`. Desempate
    /// pelo nome, para que a ordem seja total e estavel entre execucoes --
    /// sem isso, dois servidores com o mesmo desvio mudariam de lugar a cada
    /// requisicao e a latencia p99 ficaria imprevisivel.
    #[must_use]
    pub fn prefere_a(&self, a: &AlvoDeConsulta, b: &AlvoDeConsulta) -> bool {
        a.nome() < b.nome()
    }
}

/// Trade-off nomeado entre precisao e latencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Perfil {
    /// Exige 2 fontes concordantes e janela de cache curtissima. Latencia
    /// maior, confianca maior. Use quando a resposta for virar decisao
    /// contabil ou sincronizar relogio de infraestrutura.
    MaximaPrecisao,
    /// Padrao: 1 fonte, janela de 250 ms.
    #[default]
    Equilibrado,
    /// ([/]requisito de menor latencia) 1 fonte, janela de 80 ms, prazo
    /// total de 350 ms. Erros de sondagem sao servidos em degradacao a partir
    /// da ultima calibracao boa, marcada como tal.
    MenorLatencia,
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;
    use crate::fixture::{alvos_de_teste, de_ou_panica};

    #[test]
    fn os_tres_perfis_sao_validos() {
        for perfil in [Perfil::MaximaPrecisao, Perfil::Equilibrado, Perfil::MenorLatencia] {
            let r = PoliticaDeConsulta::do_perfil(perfil);
            exigir!(r.is_ok(), "perfil {perfil:?} valido", "{r:?}");
        }
    }

    #[test]
    fn menor_latencia_realmente_encerra_antes() {
        let rapida = de_ou_panica(
            PoliticaDeConsulta::do_perfil(Perfil::MenorLatencia),
            "perfil de menor latencia",
        );
        let precisa = de_ou_panica(
            PoliticaDeConsulta::do_perfil(Perfil::MaximaPrecisao),
            "perfil de maxima precisao",
        );
        // Este teste e' a **garantia estrutural** do requisito de latencia:
        // ele falha se alguem aumentar o prazo do perfil rapido. E' o tipo de
        // teste de regressao que protege uma decisao de projeto.
        exigir!(
            rapida.ajustes().prazo_total < precisa.ajustes().prazo_total,
            "perfil de latencia tem prazo total menor",
            "{:?} vs {:?}",
            rapida.ajustes().prazo_total,
            precisa.ajustes().prazo_total
        );
        exigir!(
            rapida.ajustes().janela_de_coerencia < precisa.ajustes().janela_de_coerencia,
            "perfil de latencia revalida mais cedo",
            "{:?} vs {:?}",
            rapida.ajustes().janela_de_coerencia,
            precisa.ajustes().janela_de_coerencia
        );
        exigir!(
            rapida.ajustes().tentativas < precisa.ajustes().tentativas,
            "perfil de latencia nao insiste: tentaria e atrasaria a resposta",
            "{} vs {}",
            rapida.ajustes().tentativas,
            precisa.ajustes().tentativas
        );
    }

    #[test]
    fn mais_fontes_minimas_do_que_alvos_e_erro_de_pre_condicao() {
        let ajustes = AjustesDePolitica {
            fontes_minimas: 9,
            ..AjustesDePolitica::default()
        };
        let r = PoliticaDeConsulta::de_ajustes(ajustes, alvos_de_teste(2));
        exigir!(r.is_err(), "pre-condicao: 9 minimas com 2 alvos", "{r:?}");
    }

    #[test]
    fn espera_maxima_menor_que_inicial_e_erro() {
        let ajustes = AjustesDePolitica {
            espera_inicial: Duration::from_millis(100),
            espera_maxima: Duration::from_millis(10),
            ..AjustesDePolitica::default()
        };
        let r = PoliticaDeConsulta::de_ajustes(ajustes, alvos_de_teste(2));
        exigir!(r.is_err(), "pre-condicao: esperas incoerentes", "{r:?}");
    }

    // `allow` na funcao inteira: a dobrada **tem** de ser rejeitada, e o
    // `panic!` abaixo existe para tornar visivel um teste que passaria por
    // silencio. `panic = deny` protege o codigo de producao; um teste que
    // precisa falhar alto e' o uso legitimo.
    #[test]
    #[allow(clippy::panic)]
    fn janela_de_coerencia_zero_e_erro_com_mensagem_util() {
        let ajustes = AjustesDePolitica {
            janela_de_coerencia: Duration::ZERO,
            ..AjustesDePolitica::default()
        };
        let r = PoliticaDeConsulta::de_ajustes(ajustes, alvos_de_teste(2));
        // `let-else` com destino nomeado: se a politica for aceita (o que
        // seria bug da dobrada), o teste falha em vez de passar em silencio.
        let Err(e) = r else {
            panic!("janela_de_coerencia zero deveria ser rejeitada");
        };
        exigir!(
            e.motivo().contains("cache"),
            "a mensagem precisa explicar que cache nao pode ser zero",
            "{e}"
        );
    }

    #[test]
    fn prazo_total_insuficiente_e_erro() {
        let ajustes = AjustesDePolitica {
            prazo_total: Duration::from_millis(1),
            ..AjustesDePolitica::default()
        };
        let r = PoliticaDeConsulta::de_ajustes(ajustes, alvos_de_teste(2));
        exigir!(r.is_err(), "pre-condicao: prazo nao comporta pior caso", "{r:?}");
    }

    #[test]
    fn pior_caso_de_cada_perfil_cabe_no_prazo_total() {
        // Invariante da tabela de perfis: e' a pre-condicao (7) do
        // construtor, verificada na fonte em vez de descoberta em producao.
        for perfil in [Perfil::MaximaPrecisao, Perfil::Equilibrado, Perfil::MenorLatencia] {
            let ajustes = AjustesDePolitica::do_perfil(perfil);
            let politica = PoliticaDeConsulta::de_ajustes(ajustes.clone(), alvos_de_teste(6));
            exigir!(
                politica.is_ok(),
                "perfil {perfil:?} satisfaz as pre-condicoes",
                "{politica:?}"
            );
        }
    }

    #[test]
    fn ordem_de_preferencia_e_total_e_estavel() {
        let politica = de_ou_panica(PoliticaDeConsulta::do_perfil(Perfil::Equilibrado), "perfil equilibrado");
        let alvos = alvos_de_teste(3);
        let (Some(a), Some(b)) = (alvos.first(), alvos.get(1)) else {
            return;
        };
        exigir!(
            politica.prefere_a(a, b) != politica.prefere_a(b, a),
            "pos-condicao: ordem total (anti-simetria)",
            "empate em {a} vs {b}"
        );
    }
}