//! # Adaptador de entrada: a API REST sob `/api/v1`
//!
//! O unico lugar do sistema que conhece **HTTP**. Nao sabe UDP, nao sabe
//! `tokio::UdpSocket`, nao sabe o formato de 48 bytes.
//!
//! Este crate tambem hospeda o **caso de uso** (`caso_de_uso.rs`), que e' a
//! costura entre HTTP e dominio: recebe uma intencao, devolve um resultado.
//!
//! ## Prefixo de versionamento
//!
//! Todo recurso vive sob `/api/v1`. O `v1` nao e' decorativo: ele permite
//! introduzir `/api/v2` com um contrato diferente **sem quebrar** quem usa
//! `/api/v1`, e sinaliza na URL, na primeira linha de qualquer `curl`, que
//! ha um contrato em jogo.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

//! ## Roteamento
//!
//! | Metodo | Rota | Proposito |
//! |---|---|---|
//! | `GET` | `/api/v1/hora` | a Hora Legal Brasileira, em JSON |
//! | `GET` | `/api/v1/hora.txt` | a mesma hora, como texto puro |
//! | `GET` | `/api/v1/fontes` | saude de cada servidor do Observatorio |
//! | `GET` | `/api/v1/versao` | versao, commit e numero do build |
//! | `GET` | `/api/v1/saude/liveness` | vivo? (nao checa dependencias) |
//! | `GET` | `/api/v1/saude/readiness` | pronto? (exige calibracao) |
//!
//! A explicacao de `liveness` vs `readiness` esta em `docs/OPERACAO.md`.

pub mod caso_de_uso;
pub mod dto;
pub mod erro_http;
pub mod rotas;

pub use caso_de_uso::ResultadoDeConsulta;
pub use erro_http::ErroDeResposta;
pub use rotas::construir_roteador;