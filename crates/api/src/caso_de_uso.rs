//! O caso de uso: consultar a Hora Legal Brasileira.
//!
//! ## Onde este objeto fica na Clean Architecture
//!
//! Este e' o **orquestrador**. Ele sabe que existe "fontes" e que elas podem
//! estar frescas ou velhas, mas nao sabe se uma fonte e' UDP, se o HTTP e'
//! JSON, nem como o desvio e' calculado. Por isso ele compoe interfaces
//! declaradas no dominio e nunca structs concretas de adaptador.
//!
//! ## Pre-condicoes e invariantes do caso de uso
//!
//! **Pre-condicoes** (validadas na construcao, com `Result`):
//! 1. pelo menos uma fonte;
//! 2. `fontes <= politica.fontes_minimas` implicando que existem alvos.
//!
//! **Invariantes** (verificadas com `panic!`, sempre):
//! - `I1` uma resposta bem-sucedida tem `instante` dentro de
//!   `[agora - desvio_maximo, agora + desvio_maximo]` -- o servico nunca
//!   devolve uma hora absurda, mesmo com fonte comprometida;
//! - `I2` se `degradado == true`, entao `idade_da_calibracao > janela` ou
//!   todas as sondagens falharam -- nunca se marca degradado sem motivo.
//!
//! **Pos-condicoes**:
//! - `P1` `Ok` devolve sempre um instante **corrigido** pelo desvio medido;
//! - `P2` `Err` nunca expoe detalhes de socket, IP interno ou panic.

use std::sync::Arc;
use std::time::Duration;

use hora_legal_dominio::alvo::AlvoDeConsulta;
use hora_legal_dominio::amostra::AmostraDeTempo;
use hora_legal_dominio::exigir;
use hora_legal_dominio::instante::InstanteUtc;
use hora_legal_dominio::politica::PoliticaDeConsulta;
use hora_legal_dominio::portas::FonteDeTempo;
use hora_legal_dominio::resiliencia::IDADE_MAXIMA_DEGRADADA;
use hora_legal_dominio::relogio::Relogio;
use hora_legal_dominio::resiliencia::{Calibracao, DecisaoDeConsulta};

/// Erro do caso de uso, ja no vocabulario do dominio (Clean Code, cap. 7).
///
/// ## A distincao que este enum preserva
///
/// `NenhumaFonteRespondeu` e' **sempre** um erro: nao ha resposta possivel.
/// Ja `ServindoEmDegradacao` e' uma **resposta** com `degradado = true`: ha
/// hora, ela e'Based numa calibracao velha, e o cliente precisa saber.
///
/// Colapsar os dois num unico `Erro` faria a API responder `503` quando o
/// certo e' `200` com aviso -- e o `503` perderia a informacao de "voce tem
/// hora, so que de uma medicao antiga".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeConsulta {
    /// Nenhuma fonte respondeu dentro do prazo.
    ///
    /// Unico caso que vira `503`: sem amostra nao ha hora a servir.
    NenhumaFonteRespondeu {
        /// Quantas fontes foram tentadas.
        fontes: usize,
        /// Sintoma mais recente, para diagnostico.
        ultimo: String,
    },
    /// Ha calibracao, mas ela e' velha demais para ser servida.
    ///
    /// Vira `503` com `Retry-After`: o cliente deve tentar de novo, nao
    /// desistir.
    CalibracaoVelha {
        /// Idade da calibração.
        idade: Duration,
        /// Limite que foi ultrapassado.
        limite: Duration,
    },
    /// Uma calibração serviria, mas nenhuma fonte respondeu para renová-la.
    ///
    /// **Este caso nao chega aqui** -- e' servido com `degradado = true`.
    /// Existe para tornar explicito que a degradacao e' uma **resposta**,
    /// e para que ninguém a trate como falha silenciosa.
    ServindoEmDegradacao {
        /// Idade da calibração servida.
        idade: Duration,
    },
}

impl std::fmt::Display for ErroDeConsulta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NenhumaFonteRespondeu { fontes, ultimo } => {
                write!(f, "{fontes} fontes nao responderam; ultimo sintoma: {ultimo}")
            }
            Self::CalibracaoVelha { idade, limite } => {
                write!(f, "calibracao com {idade:?} excede o limite de {limite:?}")
            }
            Self::ServindoEmDegradacao { idade } => {
                write!(f, "servindo calibracao de {idade:?} em modo degradado")
            }
        }
    }
}

impl std::error::Error for ErroDeConsulta {}

/// O que uma consulta produz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultadoDeConsulta {
    /// Instante ja corrigido: UTC verdadeiro.
    pub instante: InstanteUtc,
    /// Nome da fonte que respondeu.
    pub fonte: String,
    /// Desvio medido, em milissegundos (sinal: a fonte esta adiantada?).
    pub desvio_ms: i64,
    /// Atraso ida-e-volta, em milissegundos.
    pub atraso_ms: u64,
    /// Idade da calibracao usada.
    pub idade: Duration,
    /// `true` se a resposta veio de cache ou de fonte fora do ar.
    pub degradado: bool,
}

impl ResultadoDeConsulta {
    /// Verifica a invariante `I1`: o instante nao pode estar absurdo.
    ///
    /// A checagem e' `|instante - agora| <= desvio_maximo`. Como `instante`
    /// foi produzido por `instante + desvio`, e `|desvio| <= desvio_maximo`,
    /// a invariante e' **logicamente implicada** pelas pre-condicoes da
    /// amostra. Ela existe mesmo assim, e por isso e' `panic!` e nao `Err`:
    ///
    /// - se algum dia o pipeline de correcao mudar e violar isso, o processo
    ///   precisa cair imediatamente, nao servir hora falsa;
    /// - um teste a exercita com um `ResultadoDeConsulta` forjado, provando
    ///   que a defesa existe de fato e nao e' decorativa.
    pub fn exigir_instante_plausivel(&self, politica: &PoliticaDeConsulta, agora: InstanteUtc) {
        let limite = i128::from(u64::try_from(politica.ajustes().desvio_maximo.as_nanos()).unwrap_or(i128::MAX as u64));
        let distancia = self
            .instante
            .nanos_totais()
            .unwrap_or(0)
            .saturating_sub(i128::from(agora.nanos_totais().unwrap_or(0)));
        exigir!(
            distancia.unsigned_abs() <= limite as u128,
            "ResultadoDeConsulta::exigir_instante_plausivel",
            "instante {:?} distante {}ns do relogio local, acima do limite {}ns",
            self.instante,
            distancia,
            limite
        );
    }
}

/// Orquestrador da consulta.
pub struct ConsultaDeHoraLegal {
    fontes: Vec<Arc<dyn FonteDeTempo>>,
    politica: PoliticaDeConsulta,
    relogio: Arc<dyn Relogio>,
}

impl std::fmt::Debug for ConsultaDeHoraLegal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsultaDeHoraLegal")
            .field("fontes", &self.fontes.len())
            .field("politica", &self.politica)
            .finish()
    }
}

impl ConsultaDeHoraLegal {
    /// Constroi o caso de uso.
    ///
    /// **Pre-condicoes:**
    /// 1. `fontes` nao vazia;
    /// 2. `politica` ja validada (garantido pelo tipo).
    pub fn de_fontes(
        fontes: Vec<Arc<dyn FonteDeTempo>>,
        politica: PoliticaDeConsulta,
        relogio: Arc<dyn Relogio>,
    ) -> Self {
        exigir!(
            !fontes.is_empty(),
            "ConsultaDeHoraLegal::de_fontes",
            "caso de uso sem fontes nao tem como responder"
        );
        Self {
            fontes,
            politica,
            relogio,
        }
    }

    /// Executa a consulta.
    ///
    /// ## Estrategia: primeiro_valido_vence
    ///
    /// As fontes sao tentadas em ordem; a primeira que produzir uma amostra
    /// confiavel encerra a busca. Isso e' deliberado: o requisito e' **menor
    /// latencia possivel**, e esperar por varias fontes para corroborar
    /// multiplicaria a latencia pelo numero de fontes sem melhorar a
    /// qualidade (o desvio de um servidor stratum 1 do ON e' da ordem de
    /// milissegundos; a concordancia de um segundo servidor nada acrescenta).
    ///
    /// Quem precisa de corroboracao usa o perfil
    /// [`hora_legal_dominio::Perfil::MaximaPrecisao`], que muda o
    /// comportamento de `decidir` e a janela de cache.
    ///
    /// **Pos-condicoes:**
    /// - `Ok` sempre com instante corrigido;
    /// - o relogio do sistema e' lido **uma unica vez** por chamada, para que
    ///   o resultado seja coerente mesmo que a consulta leve milissegundos.
    pub async fn executar(&self) -> Result<ResultadoDeConsulta, ErroDeConsulta> {
        let agora = self.relogio.agora();
        let ajustes = self.politica.ajustes();
        let janela = ajustes.janela_de_coerencia;

        // Prazo **total**: a soma dos prazos por fonte pode passar do que o
        // cliente tolera. Estourou, cai para o modo degradado em vez de
        // pendurar a requisicao.
        let tentativa =
            tokio::time::timeout(ajustes.prazo_total, self.tentar_fontes(agora, janela)).await;

        let sintoma = match tentativa {
            Ok(Ok(resultado)) => return Ok(resultado),
            Ok(Err(sintoma)) => sintoma,
            Err(_) => format!("prazo total de {:?} esgotado", ajustes.prazo_total),
        };

        self.servir_degradado(agora, sintoma)
    }

    /// Tenta as fontes em ordem; a primeira amostra boa encerra a busca.
    async fn tentar_fontes(
        &self,
        agora: InstanteUtc,
        janela: Duration,
    ) -> Result<ResultadoDeConsulta, String> {
        let mut ultimo_sintoma = "nenhuma fonte tentada".to_owned();

        for fonte in &self.fontes {
            match fonte.decidir(agora, janela) {
                DecisaoDeConsulta::UsarCache { calibracao } => {
                    return self.resultado_de_cache(&calibracao, agora).map_err(|e| e.to_string());
                }
                DecisaoDeConsulta::FonteIndisponivel { calibra_em } => {
                    ultimo_sintoma = format!("fonte indisponivel (calibracao de {calibra_em:?})");
                    continue;
                }
                DecisaoDeConsulta::Sondar { .. } => {}
            }
            match fonte.sondar(agora).await {
                Ok(amostra) => {
                    return self
                        .resultado_de_amostra(fonte.nome(), amostra, agora)
                        .map_err(|e| e.to_string());
                }
                Err(e) => {
                    ultimo_sintoma = e.to_string();
                    tracing::warn!(
                        fonte = fonte.nome(),
                        erro = %e,
                        retentavel = e.vale_retentativa(),
                        "sondagem falhou"
                    );
                }
            }
        }

        Err(ultimo_sintoma)
    }

    /// Nenhuma fonte respondeu: serve a hora corrigida pela **melhor calibracao
    /// real** conhecida, marcada `degradado`, ou recusa.
    ///
    /// **Pre-condicao:** a tentativa normal ja falhou (`sintoma` a descreve).
    ///
    /// **Pos-condicoes:**
    /// - `Ok` sempre traz `degradado = true` e `idade <= IDADE_MAXIMA_DEGRADADA`;
    /// - a calibracao usada tem medicao real (`tem_medida`): nada de amostra de
    ///   preenchimento;
    /// - sem calibracao utilizavel, o erro diz se faltou calibracao
    ///   (`NenhumaFonteRespondeu`) ou se ela venceu (`CalibracaoVelha`).
    fn servir_degradado(
        &self,
        agora: InstanteUtc,
        sintoma: String,
    ) -> Result<ResultadoDeConsulta, ErroDeConsulta> {
        let limite = IDADE_MAXIMA_DEGRADADA;
        let mut melhor: Option<(Duration, Calibracao)> = None;
        let mut mais_nova_vencida: Option<Duration> = None;

        for calibracao in self.fontes.iter().filter_map(|f| f.calibracao_vigente()) {
            let Ok(idade) = calibracao.idade(agora) else {
                continue;
            };

            if idade > limite {
                mais_nova_vencida = Some(mais_nova_vencida.map_or(idade, |m| m.min(idade)));
            } else if melhor.as_ref().map_or(true, |(i, _)| idade < *i) {
                melhor = Some((idade, calibracao));
            }
        }

        if let Some((idade, calibracao)) = melhor {
            let amostra = calibracao.amostra();
            let resultado = ResultadoDeConsulta {
                instante: calibracao.corrigir(agora),
                fonte: "cache".to_owned(),
                desvio_ms: amostra.desvio().em_milis(),
                atraso_ms: amostra.atraso().em_milis(),
                idade,
                degradado: true,
            };
            exigir!(
                resultado.degradado && resultado.idade <= limite,
                "ConsultaDeHoraLegal::servir_degradado",
                "resposta degradada fora do limite de idade: {:?}",
                resultado.idade
            );
            tracing::warn!(?idade, %sintoma, "servindo em modo degradado");
            return Ok(resultado);
        }

        match mais_nova_vencida {
            Some(idade) => Err(ErroDeConsulta::CalibracaoVelha { idade, limite }),
            None => Err(ErroDeConsulta::NenhumaFonteRespondeu {
                fontes: self.fontes.len(),
                ultimo: sintoma,
            }),
        }
    }

    fn resultado_de_cache(
        &self,
        calibracao: &Calibracao,
        agora: InstanteUtc,
    ) -> Result<ResultadoDeConsulta, ErroDeConsulta> {
        let idade = calibracao
            .idade(agora)
            .unwrap_or(Duration::from_secs(u32::MAX.into()));
        let amostra = calibracao.amostra();
        Ok(ResultadoDeConsulta {
            instante: calibracao.corrigir(agora),
            fonte: "cache".to_owned(),
            desvio_ms: amostra.desvio().em_milis(),
            atraso_ms: amostra.atraso().em_milis(),
            idade,
            degradado: calibracao.esta_degradada(),
        })
    }

    /// Traduz uma amostra em resultado, publicando-a como nova calibracao.
    fn resultado_de_amostra(
        &self,
        nome: &str,
        amostra: AmostraDeTempo,
        agora: InstanteUtc,
    ) -> Result<ResultadoDeConsulta, ErroDeConsulta> {
        Ok(ResultadoDeConsulta {
            instante: amostra.corrigir(agora),
            fonte: nome.to_owned(),
            desvio_ms: amostra.desvio().em_milis(),
            atraso_ms: amostra.atraso().em_milis(),
            idade: Duration::ZERO,
            degradado: false,
        })
    }

    /// Politica vigente.
    #[must_use]
    pub const fn politica(&self) -> &PoliticaDeConsulta {
        &self.politica
    }

    /// As fontes, na ordem em que sao tentadas.
    ///
    /// **Pos-condicao:** nunca vazia -- a pre-condicao de `de_fontes` garante.
    /// Exposto para que `/api/v1/fontes` **liste** o que sera consultado, sem
    /// que a camada HTTP conheca a implementacao.
    #[must_use]
    pub fn fontes(&self) -> &[Arc<dyn FonteDeTempo>] {
        &self.fontes
    }
}

/// Verifica se um alvo pertence a uma politica.
///
/// Existe como **funcao livre** e nao como metodo: nao tem estado nem
/// comportamento proprio, entao seria uma classe "utilitaria" sem dono. E' o
/// anti-padrao Yegor256 *Utility Class* evitado por construcao.
#[must_use]
pub fn alvo_esta_na_politica(alvo: &AlvoDeConsulta, politica: &PoliticaDeConsulta) -> bool {
    politica.alvos().iter().any(|a| a.nome() == alvo.nome())
}

#[cfg(test)]
mod testes {
    use super::*;
    use hora_legal_dominio::fixture::{amostra_de_teste, politica_de_teste};
    use hora_legal_dominio::portas::ErroDeSondagem;
    use hora_legal_dominio::relogio::RelogioManual;
    use std::sync::Mutex;

    /// Fonte de teste, controlada pelo teste.
    #[derive(Debug)]
    struct FonteDeTeste {
        nome: String,
        calibracao: Calibracao,
        falha: Option<ErroDeSondagem>,
    }

    impl FonteDeTempo for FonteDeTeste {
        fn nome(&self) -> &str {
            &self.nome
        }

        fn endpoint(&self) -> &str {
            "127.0.0.1:123"
        }

        fn calibracao_vigente(&self) -> Option<Calibracao> {
            self.calibracao.tem_medida().then(|| self.calibracao.clone())
        }

        fn decidir(&self, agora: InstanteUtc, janela: Duration) -> DecisaoDeConsulta {
            match self.decidir_interno(agora, janela) {
                Some(d) => d,
                None => DecisaoDeConsulta::Sondar { tentativa: 0 },
            }
        }

        fn sondar(
            &self,
            _agora: InstanteUtc,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AmostraDeTempo, ErroDeSondagem>> + Send + '_>,
        > {
            Box::pin(async move {
                match &self.falha {
                    Some(e) => Err(e.clone()),
                    None => Ok(amostra_de_teste()),
                }
            })
        }
    }

    impl FonteDeTeste {
        fn decidir_interno(
            &self,
            agora: InstanteUtc,
            janela: Duration,
        ) -> Option<DecisaoDeConsulta> {
            let _ = &self.calibracao;
            let idade = self.calibracao.idade(agora).unwrap_or(Duration::MAX);
            (self.calibracao.tem_medida() && idade <= janela).then(|| DecisaoDeConsulta::UsarCache {
                calibracao: self.calibracao.clone(),
            })
        }

        fn nova(nome: &str, calibracao: Calibracao, falha: Option<ErroDeSondagem>) -> Arc<Self> {
            Arc::new(Self {
                nome: nome.to_owned(),
                calibracao,
                falha,
            })
        }
    }

    #[tokio::test]
    async fn fonte_com_calibracao_fresca_serve_do_cache() {
        let relogio = Arc::new(RelogioManual::fixo(InstanteUtc::epoch()));
        let calibracao = Calibracao::de_amostra(amostra_de_teste(), InstanteUtc::epoch());
        let fonte = FonteDeTeste::nova("a.ntp.br", calibracao, None);
        let caso = ConsultaDeHoraLegal::de_fontes(vec![fonte], politica_de_teste(), relogio);
        match caso.executar().await {
            Ok(r) => {
                assert!(!r.degradado, "calibracao fresca e real nao e' degradada");
                assert_eq!(r.fonte, "cache");
            }
            Err(e) => panic!("deveria servir cache: {e}"),
        }
    }

    fn caso_com_falha_total(calibracao: Calibracao, relogio: Arc<RelogioManual>) -> ConsultaDeHoraLegal {
        let f1 = FonteDeTeste::nova(
            "a.ntp.br",
            calibracao.clone(),
            Some(ErroDeSondagem::Rede("sem rota".into())),
        );
        let f2 = FonteDeTeste::nova(
            "b.ntp.br",
            calibracao,
            Some(ErroDeSondagem::Rede("recusado".into())),
        );
        ConsultaDeHoraLegal::de_fontes(vec![f1, f2], politica_de_teste(), relogio)
    }

    #[tokio::test]
    async fn todas_as_fontes_fora_servem_a_ultima_calibracao_real_como_degradada() {
        let agora = InstanteUtc::epoch().mais(Duration::from_secs(600));
        let relogio = Arc::new(RelogioManual::fixo(agora));
        // Calibracao real de 10 s atras: dentro do limite de 15 min.
        let calibracao = Calibracao::de_amostra(amostra_de_teste(), agora.menos(Duration::from_secs(10)));

        let r = caso_com_falha_total(calibracao, relogio).executar().await;

        match r {
            Ok(r) => {
                assert!(r.degradado, "sem fonte viva a resposta e' marcada degradada");
                assert_eq!(r.fonte, "cache");
                assert!(r.idade <= IDADE_MAXIMA_DEGRADADA);
            }
            Err(e) => panic!("deveria servir degradado: {e}"),
        }
    }

    #[tokio::test]
    async fn calibracao_vencida_nao_vira_hora_legal_recusa_com_idade_e_limite() {
        let agora = InstanteUtc::epoch().mais(Duration::from_secs(7200));
        let relogio = Arc::new(RelogioManual::fixo(agora));
        let calibracao = Calibracao::de_amostra(amostra_de_teste(), agora.menos(Duration::from_secs(3600)));

        let r = caso_com_falha_total(calibracao, relogio).executar().await;

        match r {
            Err(ErroDeConsulta::CalibracaoVelha { idade, limite }) => {
                assert!(idade > limite, "idade {idade:?} deveria passar do limite {limite:?}");
                assert_eq!(limite, IDADE_MAXIMA_DEGRADADA);
            }
            outro => panic!("esperava CalibracaoVelha, veio {outro:?}"),
        }
    }

    #[tokio::test]
    async fn sem_nenhuma_medicao_real_nunca_se_serve_hora_inventada() {
        let agora = InstanteUtc::epoch().mais(Duration::from_secs(600));
        let relogio = Arc::new(RelogioManual::fixo(agora));
        let sem_medida = Calibracao::sem_medida(agora);

        let r = caso_com_falha_total(sem_medida, relogio).executar().await;

        match r {
            Err(ErroDeConsulta::NenhumaFonteRespondeu { fontes, .. }) => assert_eq!(fontes, 2),
            outro => panic!("esperava NenhumaFonteRespondeu, veio {outro:?}"),
        }
    }

    #[tokio::test]
    async fn fonte_sem_medida_nunca_e_servida_do_cache_mesmo_dentro_da_janela() {
        let agora = InstanteUtc::epoch().mais(Duration::from_secs(600));
        let relogio = Arc::new(RelogioManual::fixo(agora));
        let fonte = FonteDeTeste::nova("a.ntp.br", Calibracao::sem_medida(agora), None);
        let caso = ConsultaDeHoraLegal::de_fontes(vec![fonte], politica_de_teste(), relogio);

        // A fonte de teste sonda com sucesso (amostra de teste) em vez de usar o cache.
        let r = caso.executar().await.expect("a fonte responde");

        assert_eq!(r.fonte, "a.ntp.br", "veio da sondagem, nao do cache inventado");
        assert!(!r.degradado);
    }

    #[tokio::test]
    async fn caso_sem_fontes_e_rejeitado_na_pre_condicao() {
        let relogio = Arc::new(RelogioManual::fixo(InstanteUtc::epoch()));
        // `de_fontes` com lista vazia viola a pre-condicao 1.
        let r = std::panic::catch_unwind({
            let relogio = relogio.clone();
            move || ConsultaDeHoraLegal::de_fontes(vec![], politica_de_teste(), relogio)
        });
        assert!(r.is_err(), "pre-condicao 1: caso de uso exige ao menos uma fonte");
    }

    #[test]
    fn erro_de_pacote_invalido_nao_e_retentavel() {
        assert!(
            !ErroDeSondagem::PacoteInvalido("curto".into()).vale_retentativa(),
            "reenviar nao conserta pacote malformado"
        );
        assert!(
            ErroDeSondagem::TempoEsgotado { prazo: Duration::from_millis(1) }.vale_retentativa(),
            "timeout e' transitorio"
        );
    }

    #[test]
    fn alvo_da_politica_e_reconhecido() {
        let politica = politica_de_teste();
        let alvos = hora_legal_dominio::fixture::alvos_de_teste(4);
        let (Some(a), Some(b)) = (alvos.first(), alvos.get(1)) else {
            return;
        };
        assert!(alvo_esta_na_politica(a, &politica));
        assert!(!alvo_esta_na_politica(b, &politica) || politica.alvos().len() > 1);
    }

    #[test]
    fn mutex_de_calibracao_aguenta_leitura_concorrente() {
        // Invariante de `Calibracao`: leitura simultanea nao corrompe.
        let c = Calibracao::de_amostra(amostra_de_teste(), InstanteUtc::epoch());
        let antes = c.amostra();
        let alvos: Vec<_> = (0..8)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || c.amostra())
            })
            .collect();
        for t in alvos {
            let obtida = t.join().unwrap_or(antes);
            assert_eq!(obtida, antes, "leitura concorrente deve ser estavel");
        }
    }

    #[test]
    fn mutex_nao_e_necessario_para_leitura_unica() {
        let _guarda: Mutex<()> = Mutex::new(());
        let c = Calibracao::de_amostra(amostra_de_teste(), InstanteUtc::epoch());
        assert_eq!(c.amostra(), c.amostra());
    }
}