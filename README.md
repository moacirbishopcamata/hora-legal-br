# Hora Legal BR

API RESTful da **Hora Legal Brasileira**, sincronizada com o NTP.br do Observatório Nacional. Serviço pequeno em Rust:
domínio puro (sem I/O e sem dependências), cliente NTP, API HTTP e servidor.

```text
crates/dominio   núcleo puro: instante UTC, fuso, formatos (22), política, resiliência
crates/ntp       pacote NTP e sondagem das fontes
crates/api       rotas, DTO e OpenAPI
crates/servidor  binário
```

## Estado

**Em construção: `cargo build` ainda falha** (verificado em 2026-10-12). A crate `hora-legal-api` não compila: as variáveis
`HORA_LEGAL_*` são lidas com `env!` fora da crate que tem o `build.rs`, o módulo `openapi` não está declarado em
`lib.rs` e os cabeçalhos HTTP passam `&str` onde se espera `HeaderName`. O CI fica vermelho até isso ser corrigido.
O contrato das rotas abaixo vem do código das rotas e do DTO, **não** de um servidor rodando.

## Rodar

```sh
cargo run --release -p hora-legal-servidor        # escuta 0.0.0.0:8080
HORA_LEGAL_ENDERECO=127.0.0.1:8080 HORA_LEGAL_PERFIL=menor-latencia cargo run --release -p hora-legal-servidor
```

## Rotas (`/api/v1`)

| Rota | O que devolve |
|---|---|
| `GET /hora` | JSON: `utc` (RFC 3339, nanossegundos), `brasilia`, `desvio_ms`, `atraso_ms`, `fonte`, `degradado`, formatos |
| `GET /hora.txt` | a mesma hora em texto puro (RFC 3339) |
| `GET /fontes`, `GET /versao` | fontes NTP e versão |
| `GET /saude/liveness`, `GET /saude/readiness` | saúde |
| `GET /api/v1/openapi.json` | contrato OpenAPI (fora do prefixo de rotas acima) |

Contagens acima de 2^53 viajam como **string** no JSON. `degradado: true` significa resposta de cache ou fonte fora do
ar: válida, mas menos confiável.

## Quem usa

Os módulos `*_Brasil` de saúde ([`sus_brasil`](https://github.com/moacirbishopcamata/sus_brasil) e extensões) usam
este serviço como fonte única de hora para carimbar custódia, prontuário, laudo e etiqueta. Configuração:
`hora_legal_url` no `site_config.json` do Frappe (ou `HORA_LEGAL_URL`).

MIT. Ver `LICENSE`.
