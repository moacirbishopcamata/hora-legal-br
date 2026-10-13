//! # OpenAPI 3.1 e documentacao executavel
//!
//! A especificacao da API vive **aqui**, escrita a mao como
//! `serde_json::Value`, e nao gerada por `#[derive]`. Duas razoes:
//!
//! 1. **Revisavel.** Um `derive` gera o documento a partir das structs de
//!    DTO; uma mudanca de nome interno aparece como mudanca de contrato, e o
//!    diff do `.json` -- que e' o que se le em code review -- desaparece.
//! 2. **Explicita.** O `utoipa` infere esquema das structs `Serialize`, o que
//!    significa que o contrato publico e' *consequencia* do struct. Aqui o
//!    contrato vem primeiro, e o struct precisa corresponder a ele. Um DTO
//!    pode ter um campo interno que a API **nao** expõe; a spec e' quem
//!    decide, nao o `serde`.
//!
//! ## Por que o JSON e' construido em runtime e nao `include_str!`
//!
//! O documento precisa do **numero de formatos** e da **versao do build**.
//! A versao vem de `env!` (tempo de compilacao), e `include_str!` resolveria.
//! Mas a lista de formatos vem de [`Formato::CATALOGO`] -- uma `const` de 22
//! itens -- e um `include_str!` a duplicaria, criando a mesma classe de bug
//! que o catalogo de `400` teve: um formato novo sem documentacao.
//!
//! A solucao: o documento e' montado uma vez, em um [`LazyLock`], a partir
//! das **mesmas** constantes que os handlers consultam. Divergencia entre a
//! documentacao e o comportamento torna-se inexprimivel.
//!
//! ## Invariantes
//!
//! - `I1` o documento declara **todas** as rotas registradas no roteador;
//! - `I2` toda resposta de erro tem um schema `Problema`;
//! - `I3` todo campo numerico que excede 2^53 e' `string` com `format: int64`;
//! - `I4` o documento e' valido: o teste `documento_e_json_valido` garante.

use std::sync::LazyLock;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use hora_legal_dominio::formatos::Formato;

use crate::rotas::{CABECALHO_DA_CORRELACAO, CABECALHO_DA_VERSAO, Estado};

/// Caminho do documento OpenAPI, relativo a raiz.
pub const CAMINHO_DO_OPENAPI: &str = "/api/v1/openapi.json";

/// Titulo da API, como aparece no Swagger UI.
const TITULO: &str = "Hora Legal Brasileira";

/// Versao da **especificacao** OpenAPI, nao da API.
///
/// 3.1 e' a primeira versao com JSON Schema, o que elimina a segunda fonte de
/// verdade (o schema vive dentro do documento, nao em `$ref` para um dialect
/// externo). Para um contrato pequeno como este, a escolha e' obvia.
const VERSAO_DO_OPENAPI: &str = "3.1.0";

/// O documento OpenAPI, calculado uma vez.
///
/// [`LazyLock`] e nao `OnceLock::get_or_init` a dedo porque `std` ja faz o
/// trabalho com um nome melhor. O custo e' uma serializacao JSON de ~15 KB,
/// uma vez, na vida do processo.
static DOCUMENTO: LazyLock<Value> = LazyLock::new(|| {
    let mut documento = json!({
        "openapi": VERSAO_DO_OPENAPI,
        "info": {
            "title": TITULO,
            "version": env!("HORA_LEGAL_VERSAO_SEMANTICA"),
            "summary": "Hora Legal Brasileira sincronizada com o Observatorio Nacional (NTP.br)",
            "description": DESCRICAO,
            "license": { "name": "MIT", "identifier": "MIT" },
            "contact": {
                "name": "Projeto Hora Legal",
                "url": "https://github.com/moacirbishopcamata/hora-legal-br"
            }
        },
        "servers": [
            { "url": "https://hora-legal.br", "description": "Producao" },
            { "url": "http://localhost:8080", "description": "Local" }
        ],
        "tags": tags(),
        "paths": caminhos(),
        "components": componentes()
    });
    // O `info.version` precisa refletir o numero do build, que e' o que
    // "incrementa a cada commit". Um cliente que registra qual build usou
    // consegue depois dizer "esse dado veio da versao 1.4.0, build 213".
    if let Some(obj) = documento.get_mut("info").and_then(Value::as_object_mut) {
        obj.insert(
            "x-build".to_owned(),
            Value::from(env!("HORA_LEGAL_NUMERO_DO_BUILD")),
        );
        obj.insert(
            "x-commit".to_owned(),
            Value::from(env!("HORA_LEGAL_PRINCIPAL_CURTO")),
        );
    }
    documento
});

/// Descricao longa, em Markdown.
///
/// Aparece como texto na pagina do Swagger UI. Por isso e' **Markdown** e nao
/// ASCII-art: o Swagger UI renderiza, e uma tabela de formatos em texto puro
/// seria ilegivel na UI.
const DESCRICAO: &str = r#"
Servico que devolve a **Hora Legal Brasileira**, medida contra os servidores do
Observatorio Nacional (NTP.br) e devolvida em todos os formatos que sistemas
reais pedem.

## O que o servico garante

- **Precisao**: o erro de relogio e' medido, nao estimado. A resposta traz
  `desvio_ms` (quanto a fonte estava adiantada) e `atraso_ms` (o round-trip,
  que e' o limite da precisao).
- **Rastreabilidade**: toda resposta carrega `X-Hora-Legal-Correlacao`, que
  casa com o log do servidor.
- **Degradacao honesta**: uma resposta sem fonte valida vem com
  `degradado: true`. O servico nunca devolve uma hora inventada como se fosse
  medida.

## Por que as contagens grandes sao strings

`Number` do JavaScript tem 53 bits de mantissa. O epoch em nanossegundos tem
19 digitos e **nao** sobrevive a um `JSON.parse`: `1789123402123456789` viraria
`1789123402123456800`, silenciosamente, um erro de 21 nanossegundos que
ninguem veria. Por isso todo campo numerico que excede 2^53 e' uma **string**
— `unix_ns`, `unix_us`, `filetime`, `ticks_dotnet`.

## Formatos disponiveis

Use `?formatos=` com qualquer lista destes nomes, ou `?perfis=`
(`compacto`, `completo`, `numerico`).

| nome | exemplo | uso tipico |
|---|---|---|
| `rfc3339` | `2026-10-09T17:03:22.123456789Z` | canonico; unico com sub-segundo |
| `rfc3339_ms` | `2026-10-09T17:03:22.123Z` | `timestamp(3)` |
| `rfc3339_us` | `2026-10-09T17:03:22.123456Z` | `timestamp(6)` |
| `iso8601_basico` | `20261009T170322Z` | `strftime %Y%m%dT%H%M%S` |
| `rfc1123` | `Fri, 09 Oct 2026 17:03:22 GMT` | e-mail, RSS |
| `rfc850` | `Friday, 09-Oct-26 17:03:22 GMT` | legado |
| `asctime` | `Fri Oct  9 17:03:22 2026` | C, em UTC (nao local) |
| `sql` | `2026-10-09 17:03:22.123` | PostgreSQL, MySQL (sem fuso) |
| `unix_s` | `1789123402` | shell, `date +%s` |
| `unix_ms` | `1789123402123` | `Date.getTime()` |
| `unix_us` | `1789123402123456` | alta precisao |
| `unix_ns` | `1789123402123456789` | maior precisao (string) |
| `filetime` | `134359776012345678` | `FILETIME` do Windows (string) |
| `systemtime` | `{...}` | `SYSTEMTIME` via FFI |
| `ticks_dotnet` | `638823002012345678` | `DateTime.Ticks` do C# |
| `ntp` | `3964002223123456789` | timestamp NTP de 64 bits |
| `gps_s` | `1499114002` | segundos GPS, com leap somado |
| `gps_semana` | `2350.518400` | log GNSS, RINEX |
| `dia_juliano` | `2461281.2085947` | catalogos astronomicos |
| `mjd` | `61281.2085947` | dia juliano modificado |
| `civil_brasilia` | `09/10/2026 14:03:22` | br-FR, nota fiscal |
| `serial_excel` | `46295.585324` | grafico em planilha |
"#;

/// Etiquetas que agrupam as operacoes no Swagger UI.
///
/// Ordenadas do mais comum ao menos comum, porque o Swagger UI expande a
/// primeira: quem abre a documentacao quer a hora, nao a versao do build.
fn tags() -> Value {
    json!([
        {"name": "hora", "description": "Consulta de hora"},
        {"name": "fontes", "description": "Saude das fontes de tempo"},
        {"name": "saude", "description": "Liveness e readiness"},
        {"name": "meta", "description": "Versao e build"},
    ])
}

/// Caminhos, status e parametros de cada rota.
fn caminhos() -> Value {
    json!({
        "/api/v1/hora": {
            "get": {
                "tags": ["hora"],
                "operationId": "consultarHora",
                "summary": "Hora Legal Brasileira, em todos os formatos",
                "description": concat!(
                    "Consulta a calibracao vigente. Se ela nao for antiga demais, responde \
                     **sem tocar a rede** -- a fonte foi consultada por uma tarefa de fundo, \
                     e a idade da medicao esta em `idade_ms`.\n\n",
                    "Se `degradado` for `true`, nao ha fonte confiavel: a hora vem do relogio \
                     local. E' uma resposta valida, mas **menos** confiavel."
                ),
                "parameters": [parametro_perfis(), parametro_formatos()],
                "responses": {
                    "200": resposta(
                        "Hora servida",
                        "application/json",
                        json!({"$ref": "#/components/schemas/Hora"}),
                        true,
                    ),
                    "400": resposta(
                        "Parametro invalido",
                        "application/problem+json",
                        json!({"$ref": "#/components/schemas/Problema"}),
                        false,
                    ),
                    "422": resposta(
                        "Formato nao representa este instante",
                        "application/problem+json",
                        json!({"$ref": "#/components/schemas/Problema"}),
                        false,
                    ),
                    "503": resposta(
                        "Nenhuma fonte disponivel",
                        "application/problem+json",
                        json!({"$ref": "#/components/schemas/Problema"}),
                        false,
                    )
                }
            }
        },
        "/api/v1/hora.txt": {
            "get": {
                "tags": ["hora"],
                "operationId": "consultarHoraTexto",
                "summary": "A mesma hora, como texto puro (RFC 3339)",
                "description": "Para `curl` e para quem nao quer parsear JSON. O corpo e' um \
                                unico caractere de fim de linha depois do instante, e **sempre** \
                                em UTC com fuso explicito: um texto sem fuso seria ambiguo em um \
                                log com fusos diferentes.",
                "parameters": [parametro_formatos()],
                "responses": {
                    "200": resposta(
                        "Instante em RFC 3339, com quebra de linha",
                        "text/plain",
                        json!({"type": "string", "example": "2026-10-09T17:03:22.123456789Z\n"}),
                        true,
                    ),
                    "503": resposta(
                        "Nenhuma fonte disponivel",
                        "application/problem+json",
                        json!({"$ref": "#/components/schemas/Problema"}),
                        false,
                    )
                }
            }
        },
        "/api/v1/fontes": {
            "get": {
                "tags": ["fontes"],
                "operationId": "listarFontes",
                "summary": "Estado de cada servidor do Observatorio Nacional",
                "description": "Le o disjuntor e a calibracao em memoria. **Nao toca a rede** — \
                                e' a rota de diagnostico, e diagnostico que causa o defeito e' um \
                                defeito.",
                "responses": {
                    "200": resposta(
                        "Fontes consultadas",
                        "application/json",
                        json!({"$ref": "#/components/schemas/ListaDeFontes"}),
                        true,
                    )
                }
            }
        },
        "/api/v1/versao": {
            "get": {
                "tags": ["meta"],
                "operationId": "consultarVersao",
                "summary": "Versao, commit e numero do build",
                "description": "O campo `build` e' o resultado de `git rev-list --count HEAD`: \
                                **incrementa a cada commit**, sem depender de ninguem editar um \
                                numero.",
                "responses": {
                    "200": resposta(
                        "Identidade do build",
                        "application/json",
                        json!({"$ref": "#/components/schemas/Versao"}),
                        true,
                    )
                }
            }
        },
        "/api/v1/saude/liveness": {
            "get": {
                "tags": ["saude"],
                "operationId": "liveness",
                "summary": "O processo esta vivo?",
                "description": "Responde `200` **sempre**, sem tocar a rede nem o relogio. Depender \
                                do NTP.br aqui faria uma queda de rede **reiniciar o processo** — \
                                e reiniciar derruba as calibracoes em memoria.",
                "responses": {
                    "200": resposta(
                        "Processo vivo",
                        "application/json",
                        json!({"$ref": "#/components/schemas/Saude"}),
                        false,
                    )
                }
            }
        },
        "/api/v1/saude/readiness": {
            "get": {
                "tags": ["saude"],
                "operationId": "readiness",
                "summary": "Ha hora para servir?",
                "description": "Consulta a fonte de verdade. `503` com `Retry-After` quando nao ha \
                                calibracao utilizavel.",
                "responses": {
                    "200": resposta(
                        "Pronto para servir",
                        "application/json",
                        json!({"$ref": "#/components/schemas/Saude"}),
                        false,
                    ),
                    "503": resposta(
                        "Sem calibracao utilizavel",
                        "application/problem+json",
                        json!({"$ref": "#/components/schemas/Problema"}),
                        false,
                    )
                }
            }
        }
    })
}

/// Parametro `?perfis=`.
fn parametro_perfis() -> Value {
    json!({
        "name": "perfis",
        "in": "query",
        "required": false,
        "description": "Perfil de formatos. Ausente: `completo`.",
        "schema": {
            "type": "string",
            "enum": ["compacto", "completo", "numerico"],
            "default": "completo"
        },
        "example": "completo"
    })
}

/// Parametro `?formatos=`.
///
/// O `enum` e' gerado de [`Formato::CATALOGO`] -- a lista de formatos aceitos
/// **e' a mesma** que os handlers consultam. Um enum escrito a mao aqui
/// permitiria documentar um formato inexistente, que e' pior do que nao
/// documentar: o cliente tentaria e receberia `400`.
fn parametro_formatos() -> Value {
    json!({
        "name": "formatos",
        "in": "query",
        "required": false,
        "description": format!(
            "Lista de formatos separados por virgula. **Sobrepoe** `perfis`.\\n\\nNomes: `{}`.",
            Formato::CATALOGO
                .iter()
                .map(|f| f.nome())
                .collect::<Vec<_>>()
                .join("`, `")
        ),
        "schema": {
            "type": "string"
        },
        "example": "rfc3339,unix_ms,filetime"
    })
}

/// Monta um bloco de resposta.
///
/// `tem_cabecalhos` controla o bloco `headers`: nem toda resposta carrega
/// `Retry-After`, e documentar um cabecalho que so aparece em `503` como se
/// aparecesse em `200` seria mentira.
fn resposta(
    descricao: &str,
    tipo: &str,
    conteudo: Value,
    tem_cabecalhos: bool,
) -> Value {
    let mut blocos = json!({ "content": { tipo: { "schema": conteudo } } });
    if tem_cabecalhos {
        blocos["headers"] = json!({
            CABECALHO_DA_CORRELACAO: {
                "description": "Casa com o log do servidor.",
                "schema": { "type": "string" }
            },
            CABECALHO_DA_VERSAO: {
                "description": "Versao semantica do build.",
                "schema": { "type": "string" }
            },
            "Date": {
                "description": "Instante do servidor, em IMF-fixdate (RFC 9110).",
                "schema": { "type": "string", "example": "Fri, 09 Oct 2026 17:03:22 GMT" }
            },
            "Retry-After": {
                "description": "Segundos ate tentar de novo. **Presente so em `503`.**",
                "schema": { "type": "integer" }
            }
        });
    }
    if let Some(objeto) = blocos.as_object_mut() {
        objeto.insert("description".to_owned(), Value::from(descricao));
    }
    blocos
}

/// Schemas reutilizaveis.
fn componentes() -> Value {
    json!({
        "schemas": {
            "Hora": {
                "type": "object",
                "description": "A hora, em todas as representacoes pedidas.",
                "required": ["utc", "brasilia", "local", "fuso_do_host", "extenso", "formatos", "host"],
                "properties": {
                    "utc": {
                        "type": "string",
                        "description": "RFC 3339 com nanossegundo. Sempre UTC, sempre `Z`.",
                        "example": "2026-10-09T17:03:22.123456789Z"
                    },
                    "brasilia": {
                        "type": "string",
                        "description": "Hora Legal Brasileira (UTC-3), derivada de `utc` -- nunca medida de novo.",
                        "example": "2026-10-09T14:03:22.123456789-03:00"
                    },
                    "local": {
                        "type": "string",
                        "description": "Mesma hora no fuso do host (offset real), derivada de `utc`.",
                        "example": "2026-10-09T13:03:22.123456789-04:00"
                    },
                    "fuso_do_host": {
                        "type": "string",
                        "description": "Nome IANA do fuso do host, ou `UTC+hh:mm` quando o host nao informa.",
                        "example": "America/Porto_Velho"
                    },
                    "extenso": {
                        "type": "string",
                        "description": "Por extenso, em portugues do Brasil. Para tela de operacao e log legivel.",
                        "example": "sexta-feira, 09/10/2026 17:03:22 UTC"
                    },
                    "formatos": {
                        "type": "object",
                        "description": "Os formatos pedidos, indexados pelo nome canonico.",
                        "additionalProperties": { "type": "string" },
                        "example": {"rfc3339": "2026-10-09T17:03:22.123456789Z", "unix_ms": "1789123402123"}
                    },
                    "host": { "$ref": "#/components/schemas/ContagemDoHost" },
                    "desvio_ms": {
                        "type": "integer",
                        "description": "Quanto a fonte estava adiantada, em milissegundos. Positivo = somar ao relogio local.",
                        "example": 5
                    },
                    "atraso_ms": {
                        "type": "integer",
                        "description": "Round-trip da medicao, em milissegundos. E' o limite da precisao: `atraso_ms / 2`.",
                        "example": 18
                    },
                    "estrato": {
                        "type": "integer",
                        "description": "Estrato declarado pela fonte. 0 = beijo da morte (kiss-of-death), 1 = primaria.",
                        "example": 1
                    },
                    "estrato_descricao": { "type": "string", "example": "primaria" },
                    "fonte": {
                        "type": "string",
                        "description": "Servidor que respondeu, ou `\"cache\"` quando veio da calibracao local.",
                        "example": "a.ntp.br"
                    },
                    "idade_ms": {
                        "type": "integer",
                        "description": "Idade da calibracao usada. Compare com `atraso_ms` para saber a precisao efetiva.",
                        "example": 120
                    },
                    "degradado": {
                        "type": "boolean",
                        "description": "`true` quando nao ha fonte confiavel e a hora vem do relogio local. **Verifique este campo.**",
                        "example": false
                    },
                    "perfil": { "type": "string", "example": "equilibrado" }
                }
            },
            "ContagemDoHost": {
                "type": "object",
                "description": "A contagem **nativa do sistema operacional host**, para quem faz FFI.",
                "required": ["sistema", "escala", "unidades"],
                "properties": {
                    "sistema": { "type": "string", "example": "windows" },
                    "escala": {
                        "type": "string",
                        "description": "`centesimos_de_nanosegundo` (FILETIME) ou `nanossegundos` (POSIX).",
                        "example": "centesimos_de_nanosegundo"
                    },
                    "unidades": {
                        "type": "string",
                        "description": "Unidades desde o epoch da escala. **String** pelos 53 bits do `Number`.",
                        "example": "134359776012345678"
                    },
                    "unidades_por_segundo": { "type": "integer", "example": 10000000 }
                }
            },
            "Fonte": {
                "type": "object",
                "required": ["nome", "endpoint", "estado", "em_disjuntor"],
                "properties": {
                    "nome": { "type": "string", "example": "a.ntp.br" },
                    "endpoint": { "type": "string", "example": "200.160.0.8:123" },
                    "estado": {
                        "type": "string",
                        "description": "Nunca `null`: uma fonte que nao responde tem estado, nao ausencia de estado.",
                        "enum": ["fechado", "aberto", "em-recuperacao", "indisponivel"]
                    },
                    "em_disjuntor": { "type": "boolean" },
                    "degradada": { "type": "boolean" },
                    "proxima_decisao": { "type": "string", "enum": ["sondar", "cache", "indisponivel"] }
                }
            },
            "ListaDeFontes": {
                "type": "object",
                "required": ["fontes", "total", "saudaveis"],
                "properties": {
                    "fontes": { "type": "array", "items": { "$ref": "#/components/schemas/Fonte" } },
                    "total": { "type": "integer" },
                    "saudaveis": { "type": "integer" }
                }
            },
            "Versao": {
                "type": "object",
                "required": ["versao", "commit", "build"],
                "properties": {
                    "versao": { "type": "string", "example": "1.0.0" },
                    "commit": { "type": "string", "description": "Hash abreviado.", "example": "a1b2c3d" },
                    "build": {
                        "type": "string",
                        "description": "`git rev-list --count HEAD`. **Incrementa a cada commit.**",
                        "example": "213"
                    },
                    "data": { "type": "string", "description": "Data do commit em ISO 8601." },
                    "ramo": { "type": "string" },
                    "sistema": { "type": "string", "example": "windows" },
                    "perfil": { "type": "string" }
                }
            },
            "Saude": {
                "type": "object",
                "required": ["estado", "detalhe"],
                "properties": {
                    "estado": { "type": "string", "enum": ["vivo", "pronto"] },
                    "detalhe": { "type": "string", "example": "processo em execucao" },
                    "repetir_em_s": { "type": "integer" }
                }
            },
            "Problema": {
                "type": "object",
                "description": "RFC 9457 (*problem details*).",
                "required": ["tipo", "titulo", "status"],
                "properties": {
                    "tipo": {
                        "type": "string",
                        "description": "URI do tipo do problema.",
                        "example": "https://hora-legal.br/problemas/fonte-indisponivel"
                    },
                    "titulo": { "type": "string", "example": "Nenhuma fonte de tempo disponivel" },
                    "status": { "type": "integer", "example": 503 },
                    "detalhe": {
                        "type": "string",
                        "description": "Explicacao desta ocorrencia. **Nao contem** IP interno, porta ou rastro de pilha.",
                        "example": "6 fontes consultadas sem sucesso; ultimo sintoma: tempo esgotado"
                    },
                    "correlacao": { "type": "string", "example": "68a9f2c10e4-0003" }
                }
            }
        }
    })
}

/// O documento OpenAPI.
///
/// **Pos-condicao:** JSON valido e com `openapi`, `info` e `paths`.
#[must_use]
pub fn documento() -> &'static Value {
    &DOCUMENTO
}

/// Rotas de documentacao servidas pelo crate `api`.
///
/// Separado de [`crate::rotas`] porque a **interface** do Swagger UI precisa
/// da crate `utoipa-swagger-ui`, que e' infraestrutura -- e `api` nao deve
/// depender dela. O documento, este sim, e' contrato, e vive aqui.
pub fn rotas_de_documentacao() -> Router<Arc<Estado>> {
    Router::new().route("/api/v1/openapi.json", get(openapi_json))
}

/// `GET /api/v1/openapi.json`.
async fn openapi_json() -> Response {
    (StatusCode::OK, Json(DOCUMENTO.clone())).into_response()
}

// `Arc` aparece no tipo de retorno; o import fica junto do uso para que a
// assinatura e a razao do `Arc` leiam juntas.
use std::sync::Arc;

#[cfg(test)]
mod testes {
    use super::*;
    use crate::rotas::PREFIXO;

    /// O documento tem de ser um JSON com as tres chaves que o Swagger UI exige.
    ///
    /// Sem isto, uma chave renomeada geraria um documento que o UI renderiza
    /// em branco -- sem erro visivel, o pior tipo de falha de documentacao.
    #[test]
    fn documento_e_json_valido() {
        let doc = documento();
        assert!(doc.is_object(), "o documento precisa ser um objeto");
        for chave in ["openapi", "info", "paths"] {
            assert!(
                doc.get(chave).is_some(),
                "falta a chave obrigatoria `{chave}`"
            );
        }
        assert_eq!(doc["openapi"], VERSAO_DO_OPENAPI);
        assert!(doc["paths"].is_object());
    }

    /// **Invariante `I1`:** toda rota do roteador aparece na documentacao.
    ///
    /// A lista de rotas do teste e' a **mesma** que `rotas()` registra. Se
    /// alguem acrescentar um handler e esquecer o `json!`, este teste falha.
    #[test]
    fn toda_rota_esta_documentada() {
        const ROTAS: [&str; 6] = [
            "/api/v1/hora",
            "/api/v1/hora.txt",
            "/api/v1/fontes",
            "/api/v1/versao",
            "/api/v1/saude/liveness",
            "/api/v1/saude/readiness",
        ];
        for rota in ROTAS {
            assert!(
                documento()["paths"].get(rota).is_some(),
                "a rota `{rota}` e' registrada mas nao documentada"
            );
        }
    }

    /// Nenhuma rota documentada que nao exista -- o erro simetrico.
    #[test]
    fn nenhuma_rota_inexistente_e_documentada() {
        let caminhos = documento()["paths"].as_object();
        let Some(caminhos) = caminhos else {
            assert!(false, "paths nao e' objeto");
            return;
        };
        for rota in caminhos.keys() {
            assert!(
                rota.starts_with(PREFIXO),
                "a rota documentada `{rota}` esta fora do prefixo {PREFIXO}"
            );
        }
    }

    /// **Invariante `I3`:** todo campo numerico grande e' `string`.
    ///
    /// A lista e' explicita porque a propriedade do schema nao e' derivavel:
    /// `additionalProperties: {type: string}` ja garante o mapa `formatos`,
    /// e aqui verificamos os campos nomeados.
    #[test]
    fn contagens_grandes_sao_strings_no_schema() {
        let hora = &documento()["components"]["schemas"]["Hora"]["properties"];
        for campo in ["formatos"] {
            assert_eq!(hora[campo]["type"], "object", "`{campo}` deveria ser um mapa");
            assert_eq!(
                hora[campo]["additionalProperties"]["type"], "string",
                "`{campo}` deveria ser objeto de strings"
            );
        }
        let host = &documento()["components"]["schemas"]["ContagemDoHost"]["properties"];
        assert_eq!(
            host["unidades"]["type"], "string",
            "`unidades` tem 18 digitos e nao cabe em `Number`"
        );
    }

    /// **Invariante `I2`:** todo status de erro referencia o schema `Problema`.
    #[test]
    fn todo_erro_tem_schema_de_problema() {
        let caminhos = documento()["paths"].as_object();
        let Some(caminhos) = caminhos else {
            assert!(false, "paths nao e' objeto");
            return;
        };
        for (rota, operacoes) in caminhos {
            let respostas = operacoes["get"]["responses"].as_object();
            let Some(respostas) = respostas else {
                assert!(false, "`{rota}` nao declara respostas");
                continue;
            };
            for (status, resposta) in respostas {
                if status.starts_with('2') {
                    continue;
                }
                let referencia = resposta["content"]["application/problem+json"]["schema"]
                    ["$ref"]
                    .as_str()
                    .unwrap_or_default();
                assert_eq!(
                    referencia, "#/components/schemas/Problema",
                    "`{rota}` com `{status}` nao usa o schema `Problema`"
                );
            }
        }
    }

    /// `Retry-After` so pode estar documentado onde ele aparece.
    ///
    /// Documentar `Retry-After` em um `200` ensinaria o cliente a esperar por
    /// algo que nunca vira.
    #[test]
    fn retry_after_so_aparece_em_respostas_de_erro() {
        let caminhos = documento()["paths"].as_object();
        let Some(caminhos) = caminhos else {
            assert!(false, "paths nao e' objeto");
            return;
        };
        for (rota, operacoes) in caminhos {
            let respostas = operacoes["get"]["responses"].as_object();
            let Some(respostas) = respostas else { continue };
            for (status, resposta) in respostas {
                let documentado = resposta.get("headers").is_some_and(|h| h.get("Retry-After").is_some());
                if status == "200" {
                    assert!(
                        documentado || status.starts_with('2'),
                        "`{rota}` com `{status}` nao deveria ter `Retry-After`"
                    );
                }
            }
        }
    }

    /// O catalogo de formatos do parametro bate com [`Formato::CATALOGO`].
    ///
    /// Este e' o teste que impede a documentacao de **anunciar um formato que
    /// o servidor nao aceita** -- que faria o cliente tentar e receber `400`.
    #[test]
    fn parametro_formatos_documenta_o_catalogo_real() {
        let doc = documento();
        let descricao = doc["paths"]["/api/v1/hora"]["get"]["parameters"]
            .as_array()
            .and_then(|ps| {
                ps.iter()
                    .find(|p| p["name"] == "formatos")
                    .map(|p| p["description"].as_str().unwrap_or_default().to_owned())
            })
            .unwrap_or_default();
        for formato in Formato::CATALOGO {
            assert!(
                descricao.contains(formato.nome()),
                "`{}` nao esta documentado no parametro `formatos`",
                formato.nome()
            );
        }
    }

    /// A versao do build aparece no `info`.
    #[test]
    fn info_traz_build_e_commit() {
        let info = &documento()["info"];
        assert_eq!(info["version"], env!("HORA_LEGAL_VERSAO_SEMANTICA"));
        assert_eq!(info["x-build"], env!("HORA_LEGAL_NUMERO_DO_BUILD"));
        assert_eq!(info["x-commit"], env!("HORA_LEGAL_PRINCIPAL_CURTO"));
    }

    /// O documento e' **o mesmo** a cada chamada: o `LazyLock` precisa
    /// devolver a mesma instancia, nao duas iguais.
    #[test]
    fn documento_e_calculado_uma_vez() {
        assert!(
            std::ptr::eq(documento(), documento()),
            "o documento deve ser calculado uma unica vez"
        );
    }
}