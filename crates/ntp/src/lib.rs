//! # Adaptador de saida: sondagem NTP sobre UDP
//!
//! Este e' o unico lugar do sistema que conhece **sockets**, **bytes** e
//! **tempo de espera de rede**. O dominio nao sabe que UDP existe.
//!
//! ## Divisao de responsabilidade com o dominio
//!
//! | Camada | Sabe | Nao sabe |
//! |---|---|---|
//! | `hora-legal-dominio` | matematica do NTP, politica, contratos | UDP, tokio, bytes |
//! | `hora-legal-ntp` (esta) | UDP, 48 bytes, timeouts | politica, escolha de fonte |
//!
//! Este adaptador **decodifica e mede**; ele **nao julga**. A pergunta "esta
//! amostra e' confiavel?" pertence a [`hora_legal_dominio::amostra::AmostraDeTempo::de_bruta`].
//! Separar as duas coisas e' o que permite testar a politica sem abrir socket.
//!
//! ## Formatacao e latencia
//!
//! O caminho quente -- `consultar_um` -- nao aloca alem do necessario e nao
//! toca o disco. Ver `docs/LATENCIA.md`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod pacote;
pub mod sondagem;

pub use pacote::{ErroDeDecodificacao, PacoteNtp};
pub use sondagem::{ErroDeSondagem, FonteNtp, ResultadoDeSondagem};