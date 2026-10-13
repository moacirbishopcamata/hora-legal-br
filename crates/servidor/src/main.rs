//! Binario do servidor: **raiz de composicao** (Composition Root).
//!
//! ## Por que um `main.rs` com logica
//!
//! Este arquivo nao contem regra de negocio. Ele faz tres coisas, e so:
//!
//! 1. le configuracao do ambiente;
//! 2. monta o grafo de objetos, escolhendo implementacoes concretas;
//! 3. sobe o servidor e trata o desligamento.
//!
//! A Clean Code (cap. 5, *Shape* e *Boundaries*) chama isto de `Main` como
//! **Shape** do sistema: e' o unico lugar que sabe todos os nomes concretos.
//! Se uma regra de negocio entrar aqui, a arquitetura quebra -- porque agora
//! o dominio depende deste binario.
//!
//! ## Grafo de dependencias
//!
//! ```text
//!   PoliticaDeConsulta ............ (dominio)  <- configuracao
//!   FonteNtp x N ................. (ntp)      <- sockets reais
//!   ConsultaDeHoraLegal .......... (api)      <- caso de uso
//!   rotas::construir_roteador .... (api)      <- HTTP
//! ```

use std::net::SocketAddr;

use hora_legal_api::rotas::{ConfiguracaoDeHttp, construir_roteador};
use hora_legal_dominio::{Perfil, PoliticaDeConsulta, RelogioDoSistema};
use hora_legal_ntp::FonteNtp;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), ErroDeInicializacao> {
    inicializar_observabilidade();

    let endereco = endereco_de_escuta();
    let perfil = Perfil::do_nome(&variavel("HORA_LEGAL_PERFIL").unwrap_or_else(|| "menor-latencia".into()))
        .map_err(ErroDeInicializacao::Configuracao)?;

    tracing::info!(
        perfil = ?perfil,
        versao = env!("HORA_LEGAL_VERSAO_SEMANTICA"),
        commit = env!("HORA_LEGAL_PRINCIPAL_CURTO"),
        build = env!("HORA_LEGAL_NUMERO_DO_BUILD"),
        "hora-legal: iniciando"
    );

    let politica = PoliticaDeConsulta::do_perfil(perfil)
        .map_err(ErroDeInicializacao::Configuracao)?;

    // Ponto unico onde UDP vira implementacao concreta. Trocar o transporte
    // (por NTS, por exemplo) e' mudar esta linha -- e nada mais.
    let relogio = RelogioDoSistema::new();
    let fontes = politica
        .alvos()
        .iter()
        .map(|alvo| FonteNtp::de_alvo(alvo.clone(), relogio))
        .collect::<Vec<_>>();

    let caso_de_uso = hora_legal_api::caso_de_uso::ConsultaDeHoraLegal::de_fontes(
        fontes,
        politica,
        relogio,
    );

    let aplicacao = construir_roteador(ConfiguracaoDeHttp {
        caso_de_uso: caso_de_uso.into(),
        versao: IdentidadeDoBuild::atual(),
    });

    let escuta = TcpListener::bind(endereco)
        .await
        .map_err(|e| ErroDeInicializacao::Escuta(format!("{endereco}: {e}")))?;

    tracing::info!(%endereco, "hora-legal: escutando");

    axum::serve(escuta, aplicacao)
        .with_graceful_shutdown(despistar_encerramento())
        .await
        .map_err(|e| ErroDeInicializacao::Servidor(e.to_string()))?;

    tracing::info!("hora-legal: encerrado comCleaning");
    Ok(())
}

/// Falha na subida do servico.
///
/// ### Erros de inicializacao nao sao `Box<dyn Error>`
///
/// Um `main` que retorna `Box<dyn Error>` esconde a *natureza* da falha: o
/// operador ve "erro" sem saber se e' config, porta ocupada ou politica
/// invalida. Este enum nomeia as causas, e `Display` diz o que fazer. E' o
/// anti-padrao Yegor256 *"Catch and Return Null"* aplicado a `main`.
#[derive(Debug)]
enum ErroDeInicializacao {
    /// Configuracao invalida (perfil, politica, porta).
    Configuracao(String),
    /// Nao foi possivel ocupar a porta.
    Escuta(String),
    /// O servidor caiu em execucao.
    Servidor(String),
}

impl std::fmt::Display for ErroDeInicializacao {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuracao(m) => write!(f, "configuracao invalida: {m}"),
            Self::Escuta(m) => write!(f, "nao foi possivel escutar: {m}"),
            Self::Servidor(m) => write!(f, "servidor interrompido: {m}"),
        }
    }
}

impl std::error::Error for ErroDeInicializacao {}

/// Identidade do build, injetada por `build.rs`.
///
///## A regra de incremento por commit
///
/// `HORA_LEGAL_NUMERO_DO_BUILD` e' `git rev-list --count HEAD`: um inteiro que
/// o git garante monotonico e que muda exatamente uma unidade a cada commit.
/// A versao servida em `/api/v1/versao` e' portanto muda em todo commit sem
/// que ninguem precise lembrar de editar um arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IdentidadeDoBuild {
    versao: &'static str,
    commit: &'static str,
    numero: &'static str,
    data: &'static str,
    ramo: &'static str,
}

impl IdentidadeDoBuild {
    /// Le as variaveis injetadas na compilacao.
    const fn atual() -> Self {
        Self {
            versao: env!("HORA_LEGAL_VERSAO_SEMANTICA"),
            commit: env!("HORA_LEGAL_PRINCIPAL_CURTO"),
            numero: env!("HORA_LEGAL_NUMERO_DO_BUILD"),
            data: env!("HORA_LEGAL_DATA_DO_COMMIT"),
            ramo: env!("HORA_LEGAL_RAMO"),
        }
    }
}

fn inicializar_observabilidade() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filtro = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,hora_legal_dominio=debug"));
    fmt().with_env_filter(filtro).init();
}

fn endereco_de_escuta() -> SocketAddr {
    variavel("HORA_LEGAL_ENDERECO")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            SocketAddr::from(([0, 0, 0, 0], 8080))
        })
}

fn variavel(nome: &str) -> Option<String> {
    std::env::var(nome).ok().filter(|v| !v.trim().is_empty())
}

/// Espera `SIGINT`/`SIGTERM` para encerrar com elegância.
///
/// `tokio::signal` so funciona em runtime com driver de sinais; o `wait`
/// duplo (Ctrl-C e terminate) cobre Windows e Unix sem `unsafe` nem FFI.
async fn despistar_encerramento() {
    letctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminar = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminar = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT recebido"),
        () = terminar => tracing::info!("SIGTERM recebido"),
    }
}