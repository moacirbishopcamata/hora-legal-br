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
use std::sync::Arc;

use hora_legal_api::caso_de_uso::ConsultaDeHoraLegal;
use hora_legal_api::rotas::{ConfiguracaoDeHttp, IdentidadeDoBuild, construir_roteador};
use hora_legal_dominio::exigir;
use hora_legal_dominio::{FonteDeTempo, Perfil, PoliticaDeConsulta, Relogio, RelogioDoSistema};
use hora_legal_ntp::FonteNtp;
use tokio::net::TcpListener;

/// Raiz de composicao.
///
/// **Pre-condicoes** (falham como `Err` nomeado, nunca como panico: vem do
/// ambiente, e o ambiente e' o mundo):
/// - `HORA_LEGAL_PERFIL`, se definido, e' um perfil conhecido;
/// - `HORA_LEGAL_ENDERECO`, se definido, e' um `ip:porta` valido;
/// - a porta de escuta pode ser ocupada.
///
/// **Invariante de resiliencia:** o servico sobe com **ao menos uma** fonte.
/// Fonte que nao inicia (socket, DNS) e' registrada e descartada, e as demais
/// seguem -- uma fonte fora nunca impede o servico. Sem nenhuma, a falha e'
/// alta e na subida, nao um `503` eterno em producao.
///
/// **Pos-condicao:** ao retornar `Ok`, o servidor encerrou por sinal, depois de
/// concluir as requisicoes em andamento.
#[tokio::main]
async fn main() -> Result<(), ErroDeInicializacao> {
    inicializar_observabilidade();

    let endereco = endereco_de_escuta()?;
    let perfil = perfil_do_ambiente()?;
    let identidade = IdentidadeDoBuild {
        versao: env!("HORA_LEGAL_VERSAO_SEMANTICA"),
        commit: env!("HORA_LEGAL_PRINCIPAL_CURTO"),
        build: env!("HORA_LEGAL_NUMERO_DO_BUILD"),
        data: env!("HORA_LEGAL_DATA_DO_COMMIT"),
        ramo: env!("HORA_LEGAL_RAMO"),
    };

    tracing::info!(
        perfil = ?perfil,
        versao = identidade.versao,
        commit = identidade.commit,
        build = identidade.build,
        "hora-legal: iniciando"
    );

    let politica = Arc::new(
        PoliticaDeConsulta::do_perfil(perfil)
            .map_err(|e| ErroDeInicializacao::Configuracao(e.to_string()))?,
    );

    // Ponto unico onde UDP vira implementacao concreta. Trocar o transporte
    // (por NTS, por exemplo) e' mudar este bloco -- e nada mais.
    let relogio: Arc<dyn Relogio> = Arc::new(RelogioDoSistema::novo());
    let mut fontes: Vec<Arc<dyn FonteDeTempo>> = Vec::new();

    for alvo in politica.alvos() {
        match FonteNtp::de_alvo_com_politica(alvo.clone(), Arc::clone(&relogio), Arc::clone(&politica))
            .await
        {
            Ok(fonte) => fontes.push(Arc::new(fonte)),
            Err(erro) => tracing::warn!(alvo = alvo.nome(), %erro, "fonte descartada na subida"),
        }
    }

    if fontes.is_empty() {
        return Err(ErroDeInicializacao::Configuracao(
            "nenhuma fonte NTP pode ser iniciada".to_owned(),
        ));
    }

    exigir!(
        !fontes.is_empty(),
        "main: pos-condicao da montagem",
        "o caso de uso so pode nascer com fontes"
    );
    tracing::info!(fontes = fontes.len(), "hora-legal: fontes ativas");

    let caso_de_uso = ConsultaDeHoraLegal::de_fontes(
        fontes,
        PoliticaDeConsulta::do_perfil(perfil)
            .map_err(|e| ErroDeInicializacao::Configuracao(e.to_string()))?,
        relogio,
    );

    let aplicacao = construir_roteador(ConfiguracaoDeHttp {
        caso_de_uso: Arc::new(caso_de_uso),
        perfil,
        versao: identidade,
    });

    let escuta = TcpListener::bind(endereco)
        .await
        .map_err(|e| ErroDeInicializacao::Escuta(format!("{endereco}: {e}")))?;

    tracing::info!(%endereco, "hora-legal: escutando");

    axum::serve(escuta, aplicacao)
        .with_graceful_shutdown(aguardar_encerramento())
        .await
        .map_err(|e| ErroDeInicializacao::Servidor(e.to_string()))?;

    tracing::info!("hora-legal: encerrado");
    Ok(())
}

/// Falha na subida do servico.
///
/// Um `main` que retorna `Box<dyn Error>` esconde a *natureza* da falha: o
/// operador ve "erro" sem saber se e' config, porta ocupada ou politica
/// invalida. Este enum nomeia as causas.
#[derive(Debug)]
enum ErroDeInicializacao {
    /// Configuracao invalida (perfil, politica, porta, fontes).
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

fn inicializar_observabilidade() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filtro = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,hora_legal_dominio=debug"));
    fmt().with_env_filter(filtro).init();
}

/// **Pre-condicao:** `HORA_LEGAL_ENDERECO`, se definida, e' `ip:porta`.
/// Valor ruim e' erro (nao cai em silencio no padrao): quem digitou errado
/// precisa saber, em vez de descobrir que o servico subiu em outra porta.
fn endereco_de_escuta() -> Result<SocketAddr, ErroDeInicializacao> {
    match variavel("HORA_LEGAL_ENDERECO") {
        None => Ok(SocketAddr::from(([0, 0, 0, 0], 8080))),
        Some(texto) => texto.parse().map_err(|e| {
            ErroDeInicializacao::Configuracao(format!("HORA_LEGAL_ENDERECO={texto:?}: {e}"))
        }),
    }
}

/// **Pre-condicao:** `HORA_LEGAL_PERFIL`, se definida, e' um perfil conhecido.
fn perfil_do_ambiente() -> Result<Perfil, ErroDeInicializacao> {
    match variavel("HORA_LEGAL_PERFIL").as_deref() {
        None | Some("menor-latencia") => Ok(Perfil::MenorLatencia),
        Some("equilibrado") => Ok(Perfil::Equilibrado),
        Some("maxima-precisao") => Ok(Perfil::MaximaPrecisao),
        Some(outro) => Err(ErroDeInicializacao::Configuracao(format!(
            "HORA_LEGAL_PERFIL={outro:?}: use menor-latencia, equilibrado ou maxima-precisao"
        ))),
    }
}

fn variavel(nome: &str) -> Option<String> {
    std::env::var(nome).ok().filter(|v| !v.trim().is_empty())
}

/// Espera `SIGINT`/`SIGTERM` para encerrar com elegancia.
async fn aguardar_encerramento() {
    let ctrl_c = async {
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
