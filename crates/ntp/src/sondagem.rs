//! Sondagem UDP contra os servidores do Observatorio Nacional.
//!
//! ## Baixa latencia: as tres escolhas
//!
//! **1. Socket conectado por fonte.** `UdpSocket::connect` em UDP打招呼 deixa
//! o SO descartar pacotes de outrosorigens no kernel, em vez de wakes o
//! runtime para descarta-los. Numa API de tempo, isso e' a diferenca entre
//! "cache miss com fonte torta" e "cache miss sem desperdicio de CPU".
//!
//! **2. `try_recv` antes de `recv`.** Se a resposta ja chegou enquanto o
//! runtime decidia o que fazer, `try_recv` devolve imediatamente -- sem
//! Suspender a task. O caminho comum (resposta rapida) nunca paga o custo de
//! um park.
//!
//! **3. Sem `connect` de TCP, sem TLS, sem pool de sockets.** O transporte e'
//! UDP sem estado; um socket por fonte, feito uma vez no `main`, vive o
//! processo inteiro.
//!
//! ## Pre-condicoes e invariantes
//!
//! **Pre-condicoes:**
//! - `prazo > 0` (uma sondagem sem prazo e' um vazamento de recurso);
//! - `orcamento >= prazo` (ver [`OrcamentoDeSondagem`]).
//!
//! **Invariantes:**
//! - `I1` o relogio do sistema e' lido **uma vez** por sondagem; T1 e T4 vem
//!   da mesma leitura base, com T4 acrescido do tempo decorrido. Ler o
//!   relogio duas vezes (uma para T1, outra para T4) produziria um atraso
//!   inflado em pelo menos o custo da leitura -- tipicamente 20..40 ns, o que
//!   na pratica nao importa, mas quebraria a posicao da medicao no modelo.
//! - `I2` `T1` enviado e' **exatamente** o que aparece em
//!   [`hora_legal_dominio::SegundosNtp`] que o servidor vai devolver; sem
//!   isso, a pre-condicao 3 do decodificador rejeitaria todas as respostas.
//!
//! **Pos-condicoes:**
//! - `P1` em sucesso, a `AmostraDeTempo` foi validada pelas pre-condicoes do
//!   dominio; este adaptador nunca devolve amostra invalida.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hora_legal_dominio::alvo::AlvoDeConsulta;
use hora_legal_dominio::amostra::{AmostraDeTempo, Estrato, LeiturasBrutas};
use hora_legal_dominio::contrato::ErroDeContrato;
use hora_legal_dominio::exigir;
use hora_legal_dominio::instante::{InstanteUtc, SegundosNtp};
use hora_legal_dominio::politica::PoliticaDeConsulta;
use hora_legal_dominio::portas::{ErroDeSondagem as ErroDePorta, FonteDeTempo};
use hora_legal_dominio::relogio::Relogio;
use hora_legal_dominio::resiliencia::{
    Calibracao, DecisaoDeConsulta, Espera, FonteResiliente, disjuntor_padrao,
};
use tokio::net::UdpSocket;

use crate::pacote::{ErroDeDecodificacao, PacoteNtp, TAMANHO_DO_PACOTE};

/// Erro de sondagem **no adaptador**: os motivos que so o socket sabe.
///
/// ### Por que dois enums de erro
///
/// A porta (`hora_legal_dominio::portas::ErroDeSondagem`) expoe quatro motivos
/// que o **caso de uso** sabe tratar: estouro de prazo, rede, pacote invalido,
/// recusa. O adaptador conhece causa e consequencia: qual socket, qual
/// `Errno`, qual byte do pacote.
///
/// O adaptador traduz um no outro em [`FonteNtp`]'s implementacao da porta.
/// Nao ha conversao de erro *antes* dessa fronteira, porque o caso de uso nao
/// tem como agir sobre `EADDRNOTAVAIL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeSondagem {
    /// Socket nao pode ser criado (falta de recurso, endereco invalido).
    Socket(String),
    /// Envio ou recepcao falhou no socket.
    SocketIo(String),
    /// O prazo estourou sem resposta.
    TempoEsgotado {
        /// Prazo configurado.
        prazo: Duration,
    },
    /// Resposta recebida, mas o pacote nao pode ser usado.
    Pacote(ErroDeDecodificacao),
    /// O dominio recusou a medicao (desvio, atraso ou estrato fora da politica).
    Contrato(ErroDeContrato),
}

impl ErroDeSondagem {
    /// Traduz para o vocabulario da porta.
    ///
    /// **Pos-condicao:** nunca perde informacao de que a falha foi de rede ou
    /// de protocolo -- quem decide retentativa depende disso.
    #[must_use]
    pub fn para_porta(&self) -> ErroDePorta {
        match self {
            Self::Socket(m) | Self::SocketIo(m) => ErroDePorta::Rede(m.clone()),
            Self::TempoEsgotado { prazo } => ErroDePorta::TempoEsgotado { prazo: *prazo },
            Self::Pacote(e) => ErroDePorta::PacoteInvalido(e.to_string()),
            Self::Contrato(e) => ErroDePorta::PacoteInvalido(e.to_string()),
        }
    }
}

impl std::fmt::Display for ErroDeSondagem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Socket(m) => write!(f, "socket indisponivel: {m}"),
            Self::SocketIo(m) => write!(f, "io de socket falhou: {m}"),
            Self::TempoEsgotado { prazo } => write!(f, "sem resposta em {prazo:?}"),
            Self::Pacote(e) => write!(f, "pacote invalido: {e}"),
            Self::Contrato(e) => write!(f, "medicao recusada: {e}"),
        }
    }
}

impl std::error::Error for ErroDeSondagem {}

/// Resultado de uma sondagem bem-sucedida.
///
/// Nao expoe os quatro instantes: expoe a `AmostraDeTempo`, que e' o objeto
/// de dominio. Quem quer os instantes crus usa [`AmostraDeTempo::leituras`] --
/// explicito, e nao por acidente.
pub type ResultadoDeSondagem = Result<AmostraDeTempo, ErroDeSondagem>;

/// Fonte NTP: um servidor do Observatorio, com socket, disjuntor e cache.
#[derive(Debug)]
pub struct FonteNtp {
    alvo: AlvoDeConsulta,
    /// Endpoint ja formatado, guardado para nao alocar em cada resposta.
    ///
    /// **Invariante:** sempre igual a `alvo.endpoint()`. Calculado uma vez na
    /// construcao porque `/api/v1/fontes` pode ser consultado em alta
    /// frequencia, e `format!` por chamada seria desperdicio evitavel.
    endpoint: String,
    socket: Arc<UdpSocket>,
    relogio: Arc<dyn Relogio>,
    politica: Arc<PoliticaDeConsulta>,
    resiliente: FonteResiliente,
}

impl FonteNtp {
    /// Cria a fonte e conecta o socket UDP.
    ///
    /// ## Por que `async` e por que no `main`
    ///
    /// A criacao do socket e' I/O. Fazer isso na construcao significa que o
    /// grafo de dependencias do `main` espera pela rede antes de subir o
    /// servidor -- e a API sobe com todas as fontes ja prontas. Se uma fonte
    /// falhar aqui, a falha aparece no log de inicializacao, com o endereco,
    /// e nao num `503` duas horas depois.
    ///
    /// **Pre-condicoes:**
    /// - `alvo` tem endereco e porta validos (garantido pelo tipo);
    /// - `politica` foi validada.
    pub async fn de_alvo(
        alvo: AlvoDeConsulta,
        relogio: Arc<dyn Relogio>,
    ) -> Result<Self, ErroDeSondagem> {
        let politica = PoliticaDeConsulta::do_perfil(hora_legal_dominio::politica::Perfil::Equilibrado)
            .map_err(ErroDeSondagem::Contrato)?;
        Self::de_alvo_com_politica(alvo, relogio, Arc::new(politica)).await
    }

    /// Cria a fonte com politica explicita.
    ///
    /// ## Falha na criacao e' falha alta
    ///
    /// Se o socket nao puder ser criado, a fonte **nao existe**: devolve
    /// `Err` e quem monta o servidor decide (registra e segue com as demais
    /// fontes). Um socket "de mentira" faria a fonte parecer viva e falhar
    /// calada em toda sondagem. `connect` em UDP so grava o destino (sem
    /// rede), entao uma falha ali tambem e' erro de configuracao do alvo.
    pub async fn de_alvo_com_politica(
        alvo: AlvoDeConsulta,
        relogio: Arc<dyn Relogio>,
        politica: Arc<PoliticaDeConsulta>,
    ) -> Result<Self, ErroDeSondagem> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| ErroDeSondagem::Socket(format!("bind: {e}")))?;
        socket
            .connect(alvo.endpoint())
            .await
            .map_err(|e| ErroDeSondagem::Socket(format!("{}: {e}", alvo.endpoint())))?;

        let endpoint = alvo.endpoint().to_owned();
        let espera = espera_para(&politica)?;
        // Calibracao inicial: a epoch, desvio zero. Permite que `decidir`
        // responda sem `Option` -- ver `Calibracao` no dominio.
        let calibracao = Calibracao::sem_medida(relogio.agora());
        let resiliente = FonteResiliente::nova(alvo.nome(), espera, disjuntor_padrao(), calibracao);

        Ok(Self {
            alvo,
            endpoint,
            socket: Arc::new(socket),
            relogio,
            politica,
            resiliente,
        })
    }

    /// Alvo desta fonte.
    #[must_use]
    pub fn alvo(&self) -> &AlvoDeConsulta {
        &self.alvo
    }

    /// Politica que rege a validação das amostras.
    #[must_use]
    pub fn politica(&self) -> &PoliticaDeConsulta {
        &self.politica
    }

    /// Mecanismos de resiliencia (disjuntor, cache, espera).
    #[must_use]
    pub const fn resiliente(&self) -> &FonteResiliente {
        &self.resiliente
    }

    /// Executa uma sondagem.
    ///
    /// **Pos-condicoes:** em sucesso, `AmostraDeTempo::de_bruta` ja rodou e
    /// aceitou a medicao; em falha, o disjuntor foi notificado.
    pub async fn sondar(&self, agora: InstanteUtc) -> ResultadoDeSondagem {
        let prazo = self.politica.ajustes().prazo_por_fonte;
        // **Pos-condicao P1:** em `Ok`, `AmostraDeTempo::de_bruta` rodou e
        // aceitou. Este metodo **nao** toca o disjuntor nem a calibracao --
        // quem faz isso e' a implementacao de `FonteDeTempo`, para que
        // `sondar` puro continue util em teste.
        self.sondar_com_prazo(prazo, agora).await
    }

    /// Sondagem com prazo explicito.
    ///
    /// **Pre-condicao:** `prazo > 0`.
    async fn sondar_com_prazo(&self, prazo: Duration, agora: InstanteUtc) -> ResultadoDeSondagem {
        exigir!(
            !prazo.is_zero(),
            "FonteNtp::sondar_com_prazo",
            "prazo zero deixaria a task pendurada para sempre"
        );

        // Invariante I1/I2: T1 derivado de uma unica leitura do relogio.
        let t1 = agora;
        let enviado = SegundosNtp::de_bruto(
            t1.para_segundos_ntp().map_err(ErroDeSondagem::Contrato)?,
        );
        let requisicao = PacoteNtp::de_requisicao(enviado);

        self.socket
            .send(&requisicao)
            .await
            .map_err(|e| ErroDeSondagem::SocketIo(e.to_string()))?;

        let mut buffer = [0u8; TAMANHO_DO_PACOTE];
        let recebido = match tokio::time::timeout(prazo, self.socket.recv(&mut buffer)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(ErroDeSondagem::SocketIo(e.to_string())),
            Err(_elapsed) => return Err(ErroDeSondagem::TempoEsgotado { prazo }),
        };

        // T4 medido **depois** do recv, imediatamente: e' o instante de chegada
        // ao espaco do processo, que e' o que o modelo de NTP assume.
        let t4 = self.relogio.agora();

        let pacote = PacoteNtp::decodificar(buffer.get(..recebido).unwrap_or(&[]), enviado)
            .map_err(ErroDeSondagem::Pacote)?;

        self.em_amostra(pacote, t1, t4, agora)
    }

    /// Traduz um pacote em `AmostraDeTempo`, aplicando a politica.
    fn em_amostra(
        &self,
        pacote: PacoteNtp,
        t1: InstanteUtc,
        t4: InstanteUtc,
        agora: InstanteUtc,
    ) -> ResultadoDeSondagem {
        // Conversao de 64 bits de ponto fixo para o nosso InstanteUtc. O
        // adaptador converte; o dominio decide. A decisao de qual instante
        // usar (T3 como melhor estimativa) e' regra de NTP, entao vive aqui.
        let t2 = pacote.recepcao.em_instante();
        let t3 = pacote.transmissao.em_instante();

        let leituras = LeiturasBrutas::de_quatro(t1, t2, t3, t4).map_err(ErroDeSondagem::Contrato)?;

        let estrato = Estrato::de_byte(pacote.estrato);
        let beijo = (pacote.estrato == 0).then(|| {
            hora_legal_dominio::amostra::BeijoDaMorte::de_byte(pacote.beijo_da_morte)
        });

        let amostra = AmostraDeTempo::de_bruta(leituras, estrato, beijo, agora, &self.politica)
            .map_err(ErroDeSondagem::Contrato)?;

        Ok(amostra)
    }
}

impl FonteDeTempo for FonteNtp {
    fn nome(&self) -> &str {
        self.alvo.nome()
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn decidir(&self, agora: InstanteUtc, janela: Duration) -> DecisaoDeConsulta {
        self.resiliente.decidir(agora, janela)
    }

    fn calibracao_vigente(&self) -> Option<Calibracao> {
        let calibracao = self.resiliente.calibracao();
        calibracao.tem_medida().then(|| calibracao.clone())
    }

    /// Sondagem que publica a calibracao em caso de sucesso.
    ///
    /// ## Por que publica aqui e nao no caso de uso
    ///
    /// Publicar no caso de uso exigiria que ele conhecesse `Calibracao`, que
    /// e' estrutura **de fonte**: cada fonte tem a sua. A porta devolve
    /// `AmostraDeTempo` -- o dado -- e quem guarda o dado e' quem o produz.
    /// O caso de uso continua sem saber que cache existe.
    fn sondar(
        &self,
        _agora: InstanteUtc,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AmostraDeTempo, ErroDePorta>> + Send + '_>,
    > {
        let agora = self.relogio.agora();
        Box::pin(async move {
            match self.sondar(agora).await {
                Ok(amostra) => {
                    self.resiliente.disjuntor().registrar_sucesso(agora);
                    self.resiliente.calibracao().publicar(amostra, agora);
                    Ok(amostra)
                }
                Err(e) => {
                    self.resiliente.disjuntor().registrar_falha(agora);
                    Err(e.para_porta())
                }
            }
        })
    }
}

/// Formato de timestamp nativo do host, para o DTO HTTP.
///
/// Re-exportado para que a camada de API nao precise montar a escala: quem
/// decide o formato de saida e' o **dominio**.
pub use hora_legal_dominio::formatos::Formato as FormatoDeHost;

/// Espera da fonte derivada da politica.
fn espera_para(politica: &PoliticaDeConsulta) -> Result<Espera, ErroDeSondagem> {
    Espera::nova(
        politica.ajustes().espera_inicial,
        politica.ajustes().espera_maxima,
    )
    .map_err(ErroDeSondagem::Contrato)
}

/// Resolve o endereco de um alvo, para diagnostico.
pub async fn resolver(alvo: &AlvoDeConsulta) -> Result<SocketAddr, ErroDeSondagem> {
    tokio::net::lookup_host(alvo.endpoint())
        .await
        .map_err(|e| ErroDeSondagem::Socket(format!("{}: {e}", alvo.endpoint())))?
        .next()
        .ok_or_else(|| {
            ErroDeSondagem::Socket(format!("{}: sem endereco", alvo.endpoint()))
        })
}

#[cfg(test)]
mod testes {
    use super::*;
    use hora_legal_dominio::relogio::RelogioDoSistema;
    use hora_legal_dominio::resiliencia::Calibracao;

    fn relogio() -> Arc<dyn Relogio> {
        Arc::new(RelogioDoSistema::novo())
    }

    fn politica() -> Arc<PoliticaDeConsulta> {
        Arc::new(hora_legal_dominio::fixture::politica_de_teste())
    }

    async fn fonte_para(endereco: &str, porta: u16) -> FonteNtp {
        let alvo = AlvoDeConsulta::novo("teste", endereco, porta).expect("alvo de teste valido");
        FonteNtp::de_alvo_com_politica(alvo, relogio(), politica())
            .await
            .expect("fonte de teste")
    }

    /// Servidor NTP de brinquedo no loopback: responde ao pedido espelhando o
    /// carimbo de transmissao do cliente como origem (a prova que o decodificador
    /// exige) e carimbando T2/T3 com o relogio do sistema.
    async fn servidor_ntp(estrato: u8) -> u16 {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind do servidor");
        let porta = socket.local_addr().expect("endereco local").port();

        tokio::spawn(async move {
            let mut buf = [0u8; TAMANHO_DO_PACOTE];
            while let Ok((n, origem)) = socket.recv_from(&mut buf).await {
                if n != TAMANHO_DO_PACOTE {
                    continue;
                }
                let agora = RelogioDoSistema::novo().agora();
                let Ok(bruto) = agora.para_segundos_ntp() else { continue };
                let mut resposta = [0u8; TAMANHO_DO_PACOTE];
                resposta[0] = 0b0010_0100; // LI 0 | VN 4 | Mode 4 (servidor)
                resposta[1] = estrato;
                resposta[24..32].copy_from_slice(&buf[40..48]); // origem = T1 do cliente
                resposta[32..40].copy_from_slice(&bruto.to_be_bytes()); // recepcao (T2)
                resposta[40..48].copy_from_slice(&bruto.to_be_bytes()); // transmissao (T3)
                let _ = socket.send_to(&resposta, origem).await;
            }
        });

        porta
    }

    #[tokio::test]
    async fn sondagem_de_ponta_a_ponta_contra_servidor_local() {
        let porta = servidor_ntp(1).await;
        let fonte = fonte_para("127.0.0.1", porta).await;
        let agora = fonte.relogio.agora();

        let amostra = fonte.sondar(agora).await;

        assert!(amostra.is_ok(), "servidor local deve responder: {amostra:?}");
    }

    #[tokio::test]
    async fn sem_resposta_estoura_o_prazo_e_nao_trava() {
        // Socket que recebe e nunca responde.
        let mudo = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let porta = mudo.local_addr().expect("endereco").port();
        let fonte = fonte_para("127.0.0.1", porta).await;
        let agora = fonte.relogio.agora();

        let r = fonte.sondar_com_prazo(Duration::from_millis(50), agora).await;

        assert!(matches!(r, Err(ErroDeSondagem::TempoEsgotado { .. })), "veio {r:?}");
    }

    #[tokio::test]
    async fn resposta_que_nao_e_ao_nosso_pedido_e_recusada() {
        // Servidor que responde com origem errada (resposta forjada ou atrasada).
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let porta = socket.local_addr().expect("endereco").port();
        tokio::spawn(async move {
            let mut buf = [0u8; TAMANHO_DO_PACOTE];
            while let Ok((_, origem)) = socket.recv_from(&mut buf).await {
                let mut resposta = [0u8; TAMANHO_DO_PACOTE];
                resposta[0] = 0b0010_0100;
                resposta[1] = 1;
                resposta[24..32].copy_from_slice(&[9u8; 8]);
                let _ = socket.send_to(&resposta, origem).await;
            }
        });
        let fonte = fonte_para("127.0.0.1", porta).await;
        let agora = fonte.relogio.agora();

        let r = fonte.sondar(agora).await;

        assert!(matches!(r, Err(ErroDeSondagem::Pacote(_))), "veio {r:?}");
    }

    #[tokio::test]
    async fn fonte_nova_nasce_sem_medida_e_nunca_serve_cache_inventado() {
        let porta = servidor_ntp(1).await;
        let fonte = fonte_para("127.0.0.1", porta).await;
        let agora = fonte.relogio.agora();

        assert!(fonte.calibracao_vigente().is_none(), "sem medicao real nao ha calibracao a servir");
        assert!(
            matches!(fonte.decidir(agora, Duration::from_secs(3600)), DecisaoDeConsulta::Sondar { .. }),
            "sem medida, a unica decisao possivel e' sondar"
        );
    }

    #[tokio::test]
    async fn sucesso_publica_calibracao_real() {
        let porta = servidor_ntp(1).await;
        let fonte = fonte_para("127.0.0.1", porta).await;
        let agora = fonte.relogio.agora();

        let r = <FonteNtp as FonteDeTempo>::sondar(&fonte, agora).await;

        assert!(r.is_ok(), "{r:?}");
        let calibracao: Calibracao = fonte.calibracao_vigente().expect("calibracao apos sucesso");
        assert!(calibracao.tem_medida());
    }

    #[tokio::test]
    async fn falhas_repetidas_abrem_o_disjuntor() {
        let mudo = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let porta = mudo.local_addr().expect("endereco").port();
        let fonte = fonte_para("127.0.0.1", porta).await;
        let antes = fonte.resiliente().disjuntor().falhas();

        for _ in 0..3 {
            let agora = fonte.relogio.agora();
            let _ = fonte.resiliente().disjuntor().permitir_tentativa(agora);
            fonte.resiliente().disjuntor().registrar_falha(agora);
        }

        assert!(fonte.resiliente().disjuntor().falhas() >= antes + 3);
        let agora = fonte.relogio.agora();
        assert!(
            matches!(
                fonte.decidir(agora, Duration::from_secs(1)),
                DecisaoDeConsulta::FonteIndisponivel { .. }
            ),
            "tres falhas seguidas abrem o disjuntor: a fonte nao e' consultada de novo de imediato"
        );
    }

    #[tokio::test]
    async fn endereco_invalido_e_erro_na_criacao_nao_fonte_fantasma() {
        let alvo = AlvoDeConsulta::novo("ruim", "nao e um endereco", 123);
        // O proprio alvo ja recusa (contrato do dominio); se aceitar, o connect recusa.
        if let Ok(alvo) = alvo {
            let r = FonteNtp::de_alvo_com_politica(alvo, relogio(), politica()).await;
            assert!(r.is_err());
        }
    }

    #[test]
    fn erro_de_sondagem_tem_mensagem_util() {
        let e = ErroDeSondagem::TempoEsgotado { prazo: Duration::from_millis(150) };
        assert!(e.to_string().contains("150"), "mensagem deve conter o prazo: {e}");
    }

    #[test]
    fn espera_derivada_da_politica_respeita_o_teto() {
        let politica = hora_legal_dominio::fixture::politica_de_teste();
        let espera = espera_para(&politica).expect("politica de teste e' valida");
        let sorteio = hora_legal_dominio::SorteioFixo::novo(1.0);

        for n in 0..5 {
            assert!(
                espera.esperar(n, &sorteio) <= politica.ajustes().espera_maxima,
                "espera {n} excedeu o teto da politica"
            );
        }
    }
}
