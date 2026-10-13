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

Verificado em 2026-10-12: `cargo build --workspace` compila e `cargo test --workspace` passa (**167 testes**: 95 de
domínio, 54 de API, 18 de NTP, incluindo sondagem real contra um servidor NTP de brinquedo no loopback). O servidor ainda
**não foi exercitado contra o NTP.br de verdade**. Pendências: o `clippy` pedântico do projeto acusa ~40 achados antigos
(o CI roda clippy sem bloquear até zerá-los) e falta teste de carga/caos.

## Resiliência (contratos)

- **Fonte que não inicia não derruba o serviço**: é registrada e descartada; sem nenhuma fonte, a subida falha alto.
- **Nada inventado é servido**: uma fonte nova nasce `sem_medida` (`Calibracao::sem_medida`) e só passa a servir cache
  depois de uma medição real. Antes disso, a decisão é sondar.
- **Fontes em paralelo**: cache primeiro; as demais são sondadas ao mesmo tempo (`JoinSet` do Tokio), a primeira
  amostra válida vence e as outras são abortadas. O pior caso é um `prazo_por_fonte`, não a soma.
- **Prazo total** por consulta (`prazo_total` da política); estourou, cai no modo degradado.
- **Modo degradado**: se nenhuma fonte responde, serve a hora corrigida pela melhor calibração **real**, com
  `degradado: true`, enquanto tiver até 15 min (`IDADE_MAXIMA_DEGRADADA`, escolha de engenharia: ~45 ms de deriva a
  50 ppm). Mais velha que isso: `503` com `CalibracaoVelha`, nunca hora com boa aparência e sem lastro.
- **Disjuntor** por fonte e espera com recuo; resposta de NTP é aceita só se espelhar o nosso carimbo T1.
- **Mutex envenenado não derruba a leitura** (os dados guardados são valores simples, sempre coerentes).

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
