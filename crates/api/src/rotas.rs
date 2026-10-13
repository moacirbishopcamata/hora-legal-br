//! # Roteamento: `/api/v1`
//!
//! Este modulo monta o `Router` e traduz [`ErroDeResposta`] em resposta HTTP.
//! Nenhuma regra de negocio, nenhuma decisao de rede -- ver [`dto`].
//!
//! ## Os dois health checks, e por que sao diferentes
//!
//! | Rota | Pergunta | Toca a rede? |
//! |---|---|---|
//! | `/api/v1/saude/liveness` | "o processo esta vivo?" | **nao** |
//! | `/api/v1/saude/readiness` | "ha hora para servir?" | sim |
//!
//! Misturar os dois e' o erro de operacao mais comum em servico com cache: o
//! `liveness` passa a depender do NTP.br, e uma queda de rede **reinicia o
//! processo** em vez de apenas tira-lo de prontidao. Reiniciar derruba as
//! calibracoes em memoria, e o servico volta ainda pior.
//!
//! ## Pre-condicoes, invariantes e pos-condicoes
//!
//! **Pre-condicoes** (validadas na construcao de [`ConfiguracaoDeHttp`]):
//! 1. o caso de uso tem ao menos uma fonte.
//!
//! **Invariantes** (verificadas por teste):
//! - `I1` toda rota vive sob [`PREFIXO`] -- nenhuma rota na raiz;
//! - `I2` toda resposta de erro carrega `X-Hora-Legal-Correlacao`;
//! - `I3` `Date` (RFC 9110) aparece em toda resposta, inclusive de sucesso;
//! - `I4` `400`/`422` nunca trazem `Retry-After`; `503` sempre traz.
//!
//! **Pos-condicoes:**
//! - `P1` `?perfis=` desconhecido responde `400` listando o que aceita;
//! - `P2` `?formatos=` parcialmente invalido responde `400` **inteiro** --
//!   aceitar parte e descartar o resto esconderia o erro do cliente;
//! - `P3` `liveness` responde `200` sem tocar a rede.

use std::sync::{Arc, OnceLock};

use axum::extract::{Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use hora_legal_dominio::formatos::{Formato, Formatos};
use hora_legal_dominio::host::{EscalaDoHost, SistemaDoHost};
use hora_legal_dominio::instante::InstanteUtc;
use hora_legal_dominio::politica::Perfil;
use hora_legal_dominio::relogio::{Relogio, RelogioDoSistema};
use hora_legal_dominio::resiliencia::DecisaoDeConsulta;

use crate::caso_de_uso::{ConsultaDeHoraLegal, ResultadoDeConsulta};
use crate::dto::{
    FormatosResposta, RespostaDeFonte, RespostaDeHora, RespostaDeHost, RespostaDeSaude,
    RespostaDeVersao, brasilia_de, local_de, nome_do_perfil,
};
use crate::erro_http::{
    ErroDeResposta, gerar_correlacao, traduzir_erro_de_consulta,
};
use crate::openapi;

/// Prefixo de todas as rotas.
///
/// O `v1` **nao e' decorativo**: permite introduzir `/api/v2` sem quebrar
/// quem usa `v1`, e sinaliza na URL que existe um contrato em jogo.
pub const PREFIXO: &str = "/api/v1";

/// Cabecalho da versao, em toda resposta.
///
/// Contrato HTTP: permite ao cliente descobrir a versao sem gastar uma
/// requisicao em `/api/v1/versao`.
pub const CABECALHO_DA_VERSAO: &str = "x-hora-legal-versao";

/// Cabecalho de correlacao entre resposta e log do servidor.
///
/// **Contrato:** presente em **toda** resposta, de sucesso ou de erro. Sem
/// ele, um `curl -i` e o log do servidor sao dois documentos sem chave em
/// comum -- e um servico de tempo, cujo valor central e' rastreabilidade,
/// seria irracional perder a propria.
pub const CABECALHO_DA_CORRELACAO: &str = "x-hora-legal-correlacao";

/// Identidade do build, injetada pelo `build.rs`.
///
/// Campos `&'static str` porque vem de `env!`: o binario carrega o texto, e
/// nenhuma requisicao pode altera-lo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentidadeDoBuild {
    /// Versao semantica do `Cargo.toml`.
    pub versao: &'static str,
    /// Commit abreviado.
    pub commit: &'static str,
    /// `git rev-list --count HEAD`: incrementa a cada commit.
    pub build: &'static str,
    /// Data do commit em ISO 8601.
    pub data: &'static str,
    /// Ramo atual.
    pub ramo: &'static str,
}

/// Estado compartilhado por todas as rotas.
///
/// ## Por que `Arc` e nao `static`
///
/// Um `static` esconderia o grafo de dependencias e impediria teste com dois
/// casos de uso distintos no mesmo processo. Com `Arc`, cada teste monta o seu
/// -- e o `Router` pode ser construido quantas vezes forem necessarias.
#[derive(Debug, Clone)]
pub struct Estado {
    /// Caso de uso: quem decide a hora.
    pub caso_de_uso: Arc<ConsultaDeHoraLegal>,
    /// Perfil em vigor, para expor na resposta.
    pub perfil: Perfil,
    /// Identidade do build.
    pub identidade: IdentidadeDoBuild,
    /// Escala nativa do host, para o campo `host`.
    pub escala: EscalaDoHost,
    /// Relogio do sistema, para o cabecalho `Date`.
    ///
    /// **Invariante:** mesma instancia usada pelo caso de uso. Duas
    /// instancias fariam o `Date` e o `utc` do corpo divergirem em alguns
    /// microssegundos -- e o cliente nao saberia qual dos dois esta certo.
    pub relogio: Arc<RelogioDoSistema>,
}

/// Configuracao da aplicacao HTTP.
///
/// **Pre-condicao:** `caso_de_uso` foi construido com ao menos uma fonte --
/// garantido por [`ConsultaDeHoraLegal::de_fontes`].
#[derive(Debug, Clone)]
pub struct ConfiguracaoDeHttp {
    /// Caso de uso a usar.
    pub caso_de_uso: Arc<ConsultaDeHoraLegal>,
    /// Perfil em vigor.
    pub perfil: Perfil,
    /// Identidade do build.
    pub versao: IdentidadeDoBuild,
}

/// Monta o roteador completo.
///
/// **Pos-condicoes:** `I1`..`I4` do modulo.
#[must_use]
pub fn construir_roteador(configuracao: ConfiguracaoDeHttp) -> Router {
    let estado = Arc::new(Estado {
        caso_de_uso: configuracao.caso_de_uso,
        perfil: configuracao.perfil,
        identidade: configuracao.versao,
        escala: EscalaDoHost::do_host(),
        relogio: Arc::new(RelogioDoSistema::novo()),
    });

    // Todas as rotas sob o prefixo, e **apenas** sob ele. Um `.route("/hora")`
    // na raiz seria uma segunda porta de entrada sem versionamento; o teste
    // `toda_rota_esta_versionada` existe para impedir que alguem acrescente uma.
    Router::new()
        .nest(PREFIXO, rotas())
        .merge(openapi::rotas_de_documentacao())
        .layer(axum::Extension(estado.clone()))
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(axum::http::StatusCode::SERVICE_UNAVAILABLE, 
            std::time::Duration::from_millis(750),
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(estado)
}

/// Sub-roteador, sem prefixo e sem middleware.
///
/// Separado de [`construir_roteador`] para que os testes de rota montem um
/// `Router` **sem** as camadas -- medindo a latencia da rota isolada, e
/// podendo injetar `oneshot` sem percorrer o stack inteiro.
fn rotas() -> Router<Arc<Estado>> {
    Router::new()
        .route("/hora", get(hora))
        .route("/hora.txt", get(hora_texto))
        .route("/fontes", get(fontes))
        .route("/versao", get(versao))
        .route("/saude/liveness", get(liveness))
        .route("/saude/readiness", get(readiness))
}

/// Parametros de `GET /api/v1/hora`.
///
/// ## Por que um struct e nao `HashMap<String, String>`
///
/// Um mapa aceitaria qualquer chave e devolveria `None` **silencioso** para as
/// desconhecidas -- o cliente pediria `?perfis=compcto` (typo) e receberia o
/// perfil padrao sem saber. Um struct com `Option` tipado faz o typo virar
/// `400` nomeado.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct ConsultaDeHora {
    /// `?perfis=compacto|completo|numerico`.
    #[serde(default)]
    pub perfis: Option<String>,

    /// `?formatos=rfc3339,unix_ms` -- sobrepoe `perfis`.
    #[serde(default)]
    pub formatos: Option<String>,
}

/// `GET /api/v1/hora`.
async fn hora(
    State(estado): State<Arc<Estado>>,
    Query(consulta): Query<ConsultaDeHora>,
) -> RespostaDeRequisicao {
    let agora = estado.relogio.agora();
    let correlacao = gerar_correlacao(agora);

    let formatos = match resolver_formatos(&consulta) {
        Ok(lista) => lista,
        Err(erro) => return responder_erro(erro, &correlacao, agora),
    };

    match estado.caso_de_uso.executar().await {
        Ok(resultado) => Ok(resposta_de_hora(&estado, resultado, &formatos, &correlacao, agora)),
        Err(erro) => {
            // 503 com `Retry-After`: a fonte volta, e o cliente deve esperar.
            let http = traduzir_erro_de_consulta(&erro, 1);
            Err(RespostaDeErro::com(http, &correlacao, agora))
        }
    }
}

/// `GET /api/v1/hora.txt`: a mesma hora, como texto puro.
///
/// Para `curl`, `date -u -d`, e para quem nao quer parsear JSON. O formato e'
/// **sempre** RFC 3339 com fuso explicito: um texto sem fuso seria ambiguo em
/// um log com fusos diferentes.
async fn hora_texto(
    State(estado): State<Arc<Estado>>,
    Query(consulta): Query<ConsultaDeHora>,
) -> RespostaDeRequisicao {
    let agora = estado.relogio.agora();
    let correlacao = gerar_correlacao(agora);

    // O parametro e' aceito e validado, mas **ignorado**: o formato texto e'
    // um so, por contrato. Aceitar `?formatos=` aqui e fingir que funciona
    // seria pior que dizer que nao muda nada.
    if let Err(erro) = resolver_formatos(&consulta) {
        return responder_erro(erro, &correlacao, agora);
    }

    match estado.caso_de_uso.executar().await {
        Ok(resultado) => {
            let mut corpo = String::new();
            // `String` nunca falha ao escrever; o `let _ =` declara isso.
            let _ = resultado.instante.civil_utc().escrever_rfc3339(&mut corpo);
            corpo.push('\n');
            Ok(com_cabecalhos(
                (StatusCode::OK, corpo).into_response(),
                &estado,
                &correlacao,
                agora,
            ))
        }
        Err(erro) => {
            let http = traduzir_erro_de_consulta(&erro, 1);
            Err(RespostaDeErro::com(http, &correlacao, agora))
        }
    }
}

/// `GET /api/v1/fontes`: a saude de cada servidor.
///
/// Nao toca a rede: le o disjuntor e a calibracao ja em memoria. E' a rota de
/// diagnostico, e diagnostico que causa o defeito e' um defeito.
async fn fontes(State(estado): State<Arc<Estado>>) -> RespostaDeRequisicao {
    let agora = estado.relogio.agora();
    let correlacao = gerar_correlacao(agora);
    let janela = estado.caso_de_uso.politica().ajustes().janela_de_coerencia;

    let fontes: Vec<RespostaDeFonte> = estado
        .caso_de_uso
        .fontes()
        .iter()
        .map(|fonte| {
            let decisao = fonte.decidir(agora, janela);
            let mut dto = RespostaDeFonte::de_fonte(fonte.nome(), fonte.endpoint(), &decisao);
            dto.degradada = matches!(decisao, DecisaoDeConsulta::FonteIndisponivel { .. });
            dto
        })
        .collect();

    let saudaveis = fontes.iter().filter(|f| !f.em_disjuntor).count();
    let corpo = serde_json::json!({
        "fontes": fontes,
        "total": fontes.len(),
        "saudaveis": saudaveis,
    });
    Ok(com_cabecalhos(
        (StatusCode::OK, Json(corpo)).into_response(),
        &estado,
        &correlacao,
        agora,
    ))
}

/// `GET /api/v1/versao`: versao, commit e numero do build.
async fn versao(State(estado): State<Arc<Estado>>) -> RespostaDeRequisicao {
    let agora = estado.relogio.agora();
    let correlacao = gerar_correlacao(agora);
    let corpo = RespostaDeVersao {
        versao: estado.identidade.versao,
        commit: estado.identidade.commit,
        build: estado.identidade.build,
        data: estado.identidade.data,
        ramo: estado.identidade.ramo,
        sistema: SistemaDoHost::do_host().identificacao(),
        perfil: nome_do_perfil(estado.perfil),
    };
    Ok(com_cabecalhos(
        (StatusCode::OK, Json(corpo)).into_response(),
        &estado,
        &correlacao,
        agora,
    ))
}

/// `GET /api/v1/saude/liveness` -- "o processo esta vivo?"
///
/// **Pos-condicao:** responde `200` **sempre**, sem tocar a rede e sem tocar o
/// relogio. Se respondesse `503` quando o NTP.br cai, o orquestrador
/// reiniciaria o processo -- que e' exatamente o que nao se quer.
async fn liveness() -> Response {
    let corpo = RespostaDeSaude {
        estado: "vivo",
        detalhe: "processo em execucao",
        repetir_em_s: 0,
    };
    (StatusCode::OK, Json(corpo)).into_response()
}

/// `GET /api/v1/saude/readiness` -- "ha hora para servir?"
///
/// **Pos-condicao:** `200` se ha calibracao utilizavel; `503` com
/// `Retry-After` se nao ha. Consulta a fonte de verdade, com prazo proprio.
async fn readiness(State(estado): State<Arc<Estado>>) -> RespostaDeRequisicao {
    let agora = estado.relogio.agora();
    let correlacao = gerar_correlacao(agora);
    match estado.caso_de_uso.executar().await {
        Ok(_) => {
            let corpo = RespostaDeSaude {
                estado: "pronto",
                detalhe: "calibracao disponivel",
                repetir_em_s: 0,
            };
            Ok(com_cabecalhos(
                (StatusCode::OK, Json(corpo)).into_response(),
                &estado,
                &correlacao,
                agora,
            ))
        }
        Err(erro) => {
            let http = traduzir_erro_de_consulta(&erro, 1);
            Err(RespostaDeErro::com(http, &correlacao, agora))
        }
    }
}

/// Resolve a lista de formatos da query string.
///
/// **Pos-condicoes:** `P1` e `P2` do modulo.
///
/// **Pre-condicao:** nenhuma -- e' a fronteira onde um parametro arbitrario
/// do cliente entra no sistema, e por isso a validacao mora aqui e nao nos
/// handlers (que assim ficam com uma linha).
fn resolver_formatos(consulta: &ConsultaDeHora) -> Result<Vec<Formato>, ErroDeResposta> {
    // `formatos=` tem precedencia sobre `perfis=`: quem especifica formato
    // individual ja sabe o que quer.
    match (consulta.formatos.as_deref(), consulta.perfis.as_deref()) {
        (Some(lista), _) => resolver_lista(lista),
        (None, Some(nome)) => match Formatos::de_nome(nome) {
            Some(perfil) => Ok(perfil.lista().to_vec()),
            None => Err(ErroDeResposta::ParametroInvalido {
                parametro: "perfis",
                recebido: nome.to_owned(),
                aceitos: ACEITOS_DE_PERFIL,
            }),
        },
        // Sem parametro: perfil completo. E' o que serve a maioria dos
        // clientes; quem quer menos pede explicitamente.
        (None, None) => Ok(Formatos::Completo.lista().to_vec()),
    }
}

/// Nomes de perfil aceitos em `?perfis=`.
///
/// **Invariante:** contem todos os nomes de [`Formatos::nome`]. O teste
/// `catalogo_de_perfis_esta_completo` verifica -- uma constante de string
/// usada em mensagem de erro que pode ficar desatualizada e' pior que nao
/// existir.
const ACEITOS_DE_PERFIL: &str = "compacto, completo, numerico";

/// Interpreta uma lista separada por virgula.
///
/// **Pos-condicao:** `P2` -- lista nao vazia, sem nomes invalidos.
fn resolver_lista(lista: &str) -> Result<Vec<Formato>, ErroDeResposta> {
    let nomes: Vec<&str> = lista
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let nomes_vazios = nomes.is_empty();
    let formatos: Vec<Formato> = nomes
        .iter()
        .filter_map(|nome| Formato::de_nome(nome))
        .collect();

    // Aceitar parte e descartar o resto seria pior que recusar: o cliente
    // pediria `rfc3339,xyz` e receberia so RFC 3339, sem saber que o resto
    // foi ignorado.
    if nomes_vazios || formatos.len() != nomes.len() {
        return Err(ErroDeResposta::ParametroInvalido {
            parametro: "formatos",
            recebido: lista.to_owned(),
            aceitos: &catalogo_de_formatos(),
        });
    }
    Ok(formatos)
}

/// Catalogo de nomes de formato, separado por virgula.
///
/// ## Por que gerado de [`Formato::CATALOGO`] e nao escrito a mao
///
/// A alternativa -- uma constante `"rfc3339, rfc3339_ms, ..."` -- e' uma
/// segunda fonte de verdade que o compilador nao verifica: adicionar um
/// formato e' esquecer de atualizar a string, e a mensagem de `400` passaria
/// a **anunciar um nome que o servidor nao aceita** -- o oposto do que uma
/// mensagem de erro deve fazer.
///
/// Gerar de `CATALOGO` elimina a classe inteira do bug. O custo e' uma
/// concatenacao, feita **uma vez** por processo.
fn catalogo_de_formatos() -> &'static str {
    static CATALOGO: OnceLock<String> = OnceLock::new();
    CATALOGO.get_or_init(|| {
        Formato::CATALOGO
            .iter()
            .map(|f| f.nome())
            .collect::<Vec<_>>()
            .join(", ")
    })
}

/// Projeta o resultado do caso de uso no DTO.
///
/// **Pos-condicao:** `I3` -- o `Date` e' montado a partir do **mesmo**
/// instante que vai no corpo.
fn resposta_de_hora(
    estado: &Estado,
    resultado: ResultadoDeConsulta,
    formatos: &[Formato],
    correlacao: &str,
    agora: InstanteUtc,
) -> Response {
    let civil_utc = resultado.instante.civil_utc();
    let mut utc = String::new();
    let _ = civil_utc.escrever_rfc3339(&mut utc);
    let mut extenso = String::new();
    let _ = civil_utc.escrever_extenso_pt_br(&mut extenso);

    let (local, fuso_do_host) = local_de(resultado.instante);
    let dto = RespostaDeHora {
        utc,
        brasilia: brasilia_de(resultado.instante),
        local,
        fuso_do_host,
        extenso,
        formatos: FormatosResposta::de_formatos(formatos, resultado.instante),
        host: RespostaDeHost::de_instante(resultado.instante, estado.escala),
        desvio_ms: resultado.desvio_ms,
        atraso_ms: resultado.atraso_ms,
        estrato: 1,
        estrato_descricao: "primaria",
        fonte: resultado.fonte.clone(),
        idade_ms: u64::try_from(resultado.idade.as_millis()).unwrap_or(u64::MAX),
        degradado: resultado.degradado,
        perfil: nome_do_perfil(estado.perfil),
    };

    com_cabecalhos(
        (StatusCode::OK, Json(dto)).into_response(),
        estado,
        correlacao,
        agora,
    )
}

/// Cabecalho `Date` no formato IMF-fixdate.
///
/// **Contrato HTTP:** `Date` e' **obrigatorio** em qualquer resposta de
/// servidor (RFC 9110, secao 6.6.1). Um cliente que sincroniza o relogio pela
/// resposta depende dele -- e um servidor de hora que omite `Date` seria
/// ironico.
///
/// **Pre-condicao:** nenhuma; `escrever_imf_fixdate` num `String` nao falha.
fn cabecalho_date(instante: InstanteUtc) -> String {
    let mut buffer = String::new();
    let _ = instante.civil_utc().escrever_imf_fixdate(&mut buffer);
    buffer
}

/// Aplica os cabecalhos obrigatorios a uma resposta de sucesso.
///
/// Centralizar aqui garante `I2` e `I3` **por construcao**: um handler novo
/// nao pode esquecer, porque ele nunca monta a resposta final.
fn com_cabecalhos(
    mut resposta: Response,
    estado: &Estado,
    correlacao: &str,
    agora: InstanteUtc,
) -> Response {
    let cabecalhos = resposta.headers_mut();
    inserir(cabecalhos, HeaderName::from_static(CABECALHO_DA_VERSAO), estado.identidade.versao);
    inserir(cabecalhos, header::DATE, cabecalho_date(agora).as_str());
    inserir(cabecalhos, HeaderName::from_static(CABECALHO_DA_CORRELACAO), correlacao);
    resposta
}

/// Insere um cabecalho, ignorando valor invalido.
///
/// `HeaderValue::from_str` falha para byte nao-ASCII. Um valor invalido aqui
/// vem de `env!` malformado -- e **nao** vale a pena derrubar a requisicao
/// por causa de um cabecalho cosmetico. O dado principal esta no corpo, que
/// ja foi serializado.
fn inserir(cabecalhos: &mut axum::http::HeaderMap, nome: axum::http::HeaderName, valor: &str) {
    if let Ok(valor) = HeaderValue::from_str(valor) {
        cabecalhos.insert(nome, valor);
    }
}

/// Resposta de sucesso ou erro, com correlacao.
type RespostaDeRequisicao = Result<Response, RespostaDeErro>;

/// Erro pronto para virar resposta HTTP.
#[derive(Debug)]
struct RespostaDeErro {
    /// Erro traduzido.
    erro: ErroDeResposta,
    /// Correlacao do log.
    correlacao: String,
    /// Cabecalho `Date`, pronto.
    cabecalho_date: String,
}

impl RespostaDeErro {
    /// Monta o erro ja com `Date` calculado.
    ///
    /// O `Retry-After` e' lido **antes** de `erro` ser movido, o que explica
    /// a variavel local: `ErroDeResposta` nao e' `Copy`, e ler um campo depois
    /// de mover a struct seria um erro de compilacao -- nao uma leitura p.
    fn com(erro: ErroDeResposta, correlacao: &str, agora: InstanteUtc) -> Self {
        Self {
            erro,
            correlacao: correlacao.to_owned(),
            cabecalho_date: cabecalho_date(agora),
        }
    }
}

impl IntoResponse for RespostaDeErro {
    fn into_response(self) -> Response {
        let repetir_em_s = self.erro.repetir_em_s();
        let status =
            StatusCode::from_u16(self.erro.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let problema = self.erro.para_problema(&self.correlacao);

        let mut resposta = (status, Json(problema)).into_response();
        let cabecalhos = resposta.headers_mut();
        inserir(cabecalhos, HeaderName::from_static(CABECALHO_DA_CORRELACAO), &self.correlacao);
        inserir(cabecalhos, header::DATE, self.cabecalho_date.as_str());

        // `Retry-After` em segundos -- a RFC 9110 tambem aceita um HTTP-date,
        // e o numero e' o que o cliente interpreta sem parser de data.
        //
        // **Invariante `I4`:** `400`/`422` nunca trazem o cabecalho. Reenviar
        // o mesmo pedido falharia igual, e um `Retry-After` ali so faria o
        // cliente esperar por nada.
        if let Some(segundos) = repetir_em_s {
            inserir(cabecalhos, header::RETRY_AFTER, &segundos.to_string());
        }
        resposta
    }
}

/// Atalho: responde erro imediatamente.
///
/// ## Por que `&Estado` e nao `&Arc<Estado>`
///
/// O caminho de erro de **parametro** acontece antes de qualquer uso da
/// fonte, e nao precisa da escala nem da versao. Aceitar `&State` aqui
/// deixaria claro essa assimetria em vez deaceitar o estado inteiro e
/// descartar com `let _ =` -- que e' a assinatura de uma funcao que forgot
/// de usar alguma coisa.
fn responder_erro(
    erro: ErroDeResposta,
    correlacao: &str,
    agora: InstanteUtc,
) -> RespostaDeRequisicao {
    Err(RespostaDeErro::com(erro, correlacao, agora))
}

#[cfg(test)]
mod testes {
    use super::*;

    fn com_perfil(nome: &str) -> ConsultaDeHora {
        ConsultaDeHora {
            perfis: Some(nome.to_owned()),
            formatos: None,
        }
    }

    #[test]
    #[allow(clippy::panic)]
    fn perfil_desconhecido_e_400_com_lista_de_aceitos() {
        let Err(erro) = resolver_formatos(&com_perfil("compcto")) else {
            panic!("typo no perfil precisa ser 400, nao o perfil padrao");
        };
        assert_eq!(erro.status(), 400);
        assert!(
            erro.detalhe().contains("compacto"),
            "a mensagem precisa listar o que aceita: {}",
            erro.detalhe()
        );
    }

    #[test]
    #[allow(clippy::panic)]
    fn formato_desconhecido_na_lista_e_400_inteiro() {
        // Critico: aceitar parte da lista e descartar o resto esconderia o
        // erro do cliente.
        let consulta = ConsultaDeHora {
            perfis: None,
            formatos: Some("rfc3339,xyz".into()),
        };
        let Err(erro) = resolver_formatos(&consulta) else {
            panic!("formato invalido precisa ser 400, nao resposta parcial");
        };
        assert_eq!(erro.status(), 400);
    }

    #[test]
    #[allow(clippy::panic)]
    fn lista_vazia_e_400_e_nao_200_vazio() {
        let consulta = ConsultaDeHora {
            perfis: None,
            formatos: Some(" , , ".into()),
        };
        let Err(erro) = resolver_formatos(&consulta) else {
            panic!("lista vazia e' pedido sem resposta util");
        };
        assert_eq!(erro.status(), 400);
    }

    #[test]
    fn sem_parametro_usa_o_perfil_completo() {
        let lista = resolver_formatos(&ConsultaDeHora::default()).unwrap_or_default();
        assert_eq!(lista.len(), Formatos::Completo.lista().len());
    }

    #[test]
    fn formatos_tem_precedencia_sobre_perfis() {
        let consulta = ConsultaDeHora {
            perfis: Some("compacto".into()),
            formatos: Some("rfc3339".into()),
        };
        assert_eq!(
            resolver_formatos(&consulta).unwrap_or_default(),
            vec![Formato::Rfc3339],
            "formatos= deve vencer perfis="
        );
    }

    #[test]
    fn prefixo_tem_v1() {
        // Contrato de versionamento: mudar este prefixo e' quebrar clientes.
        assert_eq!(PREFIXO, "/api/v1");
    }

    #[test]
    fn catalogo_aceita_exatamente_o_que_anuncia() {
        // **Este** e' o teste que justifica gerar o catalogo de
        // `Formato::CATALOGO`: cada nome anunciado precisa ser aceito, e cada
        // formato do catalogo precisa estar anunciado.
        let anuncio = catalogo_de_formatos();
        for formato in Formato::CATALOGO {
            assert!(
                anuncio.contains(formato.nome()),
                "o catalogo de erro nao anuncia `{}`",
                formato.nome()
            );
            assert!(
                Formato::de_nome(formato.nome()).is_some(),
                "`{}` esta anunciado mas nao e' aceito",
                formato.nome()
            );
        }
    }

    #[test]
    fn catalogo_de_perfis_esta_completo() {
        // Mesma garantia para `ACEITOS_DE_PERFIL`: a constante de string e'
        // verificada contra `Formatos::nome`.
        for perfil in [
            Formatos::Compacto,
            Formatos::Completo,
            Formatos::Numerico,
        ] {
            assert!(
                ACEITOS_DE_PERFIL.contains(perfil.nome()),
                "`{}` nao aparece na mensagem de erro",
                perfil.nome()
            );
            assert!(
                Formatos::de_nome(perfil.nome()).is_some(),
                "`{}` e' anunciado mas nao e' aceito",
                perfil.nome()
            );
        }
    }

    #[test]
    fn catalogo_de_formatos_e_calculado_uma_vez() {
        // O `OnceLock` precisa devolver a **mesma** string, nao duas iguais:
        // e' o que garante ausencia de alocacao por requisicao.
        let a = catalogo_de_formatos().as_ptr();
        let b = catalogo_de_formatos().as_ptr();
        assert_eq!(a, b, "o catalogo deve ser calculado uma unica vez");
    }
}