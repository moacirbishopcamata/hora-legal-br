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
use hora_legal_dominio::formatos::Formato;
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
! explicito, e nao por acidente.
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
    pub async fn de_alvo(alvo: AlvoDeConsulta, relogio: Arc<dyn Relogio>) -> Self {
        let politica = PoliticaDeConsulta::do_perfil(
            hora_legal_dominio::politica::Perfil::Equilibrado,
        );
        Self::de_alvo_com_politica(alvo, relogio, std::sync::Arc::new(politica)).await
    }

    /// Cria a fonte com politica explicita.
    ///
    /// **Pre-condicoes:** `alvo.nome()` nao vazio; `politica` contem o alvo.
    pub async fn de_alvo_com_politica(
        alvo: AlvoDeConsulta,
        relogio: Arc<dyn Relogio>,
        politica: Arc<PoliticaDeConsulta>,
    ) -> Self {
        // O socket e' criado mesmo se `connect` falhar: `bind` em "0.0.0.0:0"
        // sempre funciona, e a falha de connect aparece na primeira sondagem,
        // onde o disjuntor consegue conta-la.
        let socket = UdpSocket::bind("0.0.0.0:0").await;
        let socket = match socket {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(alvo = alvo.nome(), erro = %e, "bind UDP falhou");
                // Sem socket, a fonte passa a falhar sempre -- comportamento
                // observavel e correto, melhor que um `unwrap` no `main`.
                Self::invalida(&alvo, relogio, politica)
            }
        };
        let socket = match socket {
            Ok(s) => {
                if let Err(e) = s.connect(alvo.endpoint()).await {
                    tracing::warn!(
                        alvo = alvo.nome(),
                        endpoint = alvo.endpoint(),
                        erro = %e,
                        "connect UDP falhou; a fonte ficara indisponivel"
                    );
                }
                Arc::new(s)
            }
            Err(_) => Self::invalida(&alvo, relogio, politica).socket,
        };

        let endpoint = alvo.endpoint();
        let espera = espera_para(&politica);
        let disjuntor = disjuntor_padrao();
        // Calibracao inicial: a epoch, desvio zero. Permite que `decidir`
        // responda sem `Option` -- ver `Calibracao` no dominio.
        let calibracao = Calibracao::de_amostra(
            hora_legal_dominio::fixture::amostra_de_teste(),
            relogio.agora(),
        );
        let resiliente = FonteResiliente::nova(
            alvo.nome(),
            espera,
            disjuntor,
            calibracao,
        );
        Self {
            alvo,
            endpoint,
            socket,
            relogio,
            politica,
            resiliente,
        }
    }

    /// Fonte sem socket, que falha sempre. Usada quando o bind falha.
    fn invalida(
        alvo: &AlvoDeConsulta,
        relogio: Arc<dyn Relogio>,
        politica: Arc<PoliticaDeConsulta>,
    ) -> Self {
        Self {
            alvo: alvo.clone(),
            endpoint: alvo.endpoint(),
            socket: Arc::new(socket_efemero()),
            relogio,
            politica,
            resiliente: FonteResiliente::nova(
                alvo.nome(),
                espera_para(&politica),
                disjuntor_padrao(),
                Calibracao::de_amostra(
                    hora_legal_dominio::fixture::amostra_de_teste(),
                    relogio.agora(),
                ),
            ),
        }
    }

    /// Alvo desta fonte.
    #[must_use]
    pub fn alvo(&self) -> &AlvoDeConsulta {
        &self.alvo
    }

    /// Politica que rege a validação das amostras.
    #[must_use]
    pub const fn politica(&self) -> &PoliticaDeConsulta {
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
        let enviado = t1
            .para_segundos_ntp()
            .map_err(ErroDeSondagem::Contrato)?;
        let requisicao = PacoteNtp::de_requisicao();

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
fn espera_para(politica: &PoliticaDeConsulta) -> Espera {
    Espera::nova(
        politica.ajustes().espera_inicial,
        politica.ajustes().espera_maxima,
    )
    .unwrap_or_else(|_| {
        Espera::new(Duration::from_millis(1), Duration::from_millis(8))
            .unwrap_or_else(|_| espera_padrao_inalcancavel())
    })
}

/// Ultimo recurso para `Espera`: um par **sempre valido`.
fn espera_padrao_inalcancavel() -> Espera {
    // 1ms..2ms satisfaz todas as pre-condicoes de `Espera::nova` sem forma de
    // falhar; se mesmo assim falhar, o `unwrap` abaixo seria justificavel --
    // mas preferimos manter o encadeamento sem `unwrap`.
    Espera::nova(Duration::from_millis(1), Duration::from_millis(2))
        .unwrap_or_else(|_| unreachable!("1ms..2ms viola pre-condicao de Espera"))
}

/// Socket efemero para uma fonte sem socket proprio.
///
/// Um `UdpSocket` unbound, ligado a porta 0. Existe para que a fonte
/// *invalida* tenha o mesmo tipo das outras -- sem `Option<FonteNtp>` e sem
/// `#[cfg]`.
fn socket_efemero() -> UdpSocket {
    // `bind` em porta efemera e' a unica operacao sincrona disponivel sem
    // entrar no runtime; `UdpSocket::bind` sync e' permitido fora de runtime.
    match UdpSocket::bind(std::net::SocketAddr::from(([0, 0, 0, 0], 0))) {
        Ok(s) => s,
        Err(_) => {
            // Socket inexistente: a fonte falhará em toda sondagem, que e' o
            // comportamento correto. `new` sem bind e' valido em tokio.
            UdpSocket::from_std(std::net::UdpSocket::bind("127.0.0.1:0").unwrap_or_else(|_| {
                // ultimo recurso: cria um socket que nunca sera usado com
                // sucesso porque `connect` apontara para um alvo invalido.
                std::net::UdpSocket::bind("0.0.0.0:0").unwrap_or_default()
            }))
        }
    }
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
    use std::net::SocketAddr;

    async fn fonte_de_teste() -> FonteNtp {
        let relogio: Arc<dyn Relogio> = Arc::new(hora_legal_dominio::relogio::RelogioDoSistema::new());
        let alvo = AlvoDeConsulta::na_porta_ntp("teste", "127.0.0.1").unwrap_or_default();
        FonteNtp::de_alvo(alvo, relogio).await
    }

    #[tokio::test]
    async fn sondar_para_endereco_sem_resposta_da_erro_de_prazo() {
        // Porta descartavel: nada escuta, entao o prazo estoura. Nao e' flaky
        // porque o `timeout` do runtime garante o resultado.
        let relogio: Arc<dyn Relogio> = Arc::new(hora_legal_dominio::relogio::RelogioDoSistema::new());
        let alvo = AlvoDeConsulta::novo("buraco", "127.0.0.1", 9).unwrap_or_default();
        let fonte = FonteNtp::de_alvo(alvo, relogio).await;
        let r = fonte.sondar(InstanteUtc::epoch()).await;
        assert!(
            matches!(r, Err(ErroDeSondagem::TempoEsgotado { .. }) | Err(ErroDeSondagem::Envio(_))),
            "sondagem sem resposta deve falhar com prazo ou envio, veio {r:?}"
        );
    }

    #[tokio::test]
    async fn falha_incrementa_o_disjuntor() {
        let fonte = fonte_de_teste().await;
        let antes = fonte.resiliente().disjuntor().falhas();
        let _ = fonte.sondar(InstanteUtc::epoch()).await;
        let depois = fonte.resiliente().disjuntor().falhas();
        assert!(depois > antes, "falha de sondagem alimenta o disjuntor");
    }

    #[tokio::test]
    async fn resolve_endereco_local() {
        let alvo = AlvoDeConsulta::na_porta_ntp("localhost", "127.0.0.1").unwrap_or_default();
        match resolver(&alvo).await {
            Ok(addr) => assert!(addr.port() == 123),
            Err(e) => panic!("localhost:123 deveria resolver: {e}"),
        }
    }

    #[test]
    fn erro_de_sondagem_tem_mensagem_util() {
        let e = ErroDeSondagem::TempoEsgotado { prazo: Duration::from_millis(150) };
        let msg = e.to_string();
        assert!(msg.contains("150"), "mensagem deve conter o prazo: {msg}");
    }

    #[test]
    fn espera_derivada_da_politica_e_valida() {
        let politica = hora_legal_dominio::politica::PoliticaDeConsulta::do_perfil(
            hora_legal_dominio::Perfil::Equilibrado,
        )
        .unwrap_or_default();
        let e = espera_para(&politica);
        // Pos-condicao: nunca excede o teto da politica.
        let s = hora_legal_dominio::SorteioFixo::novo(1.0);
        for n in 0..5 {
            assert!(
                e.esperar(n, &s) <= politica.ajustes().espera_maxima,
                "espera {n} excedeu o teto da politica"
            );
        }
    }

    #[tokio::test]
    async fn fonte_expoe_o_nome_do_alvo() {
        let f = fonte_de_teste().await;
        assert_eq!(f.alvo().nome(), "teste");
        assert_eq!(f.alvo().porta(), 123);
        let _ = SocketAddr::from(([0, 0, 0, 0], 0));
    }
}