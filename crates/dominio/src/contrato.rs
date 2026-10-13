//! # Primitivas de Design by Contract
//!
//! Este modulo e' o **unico** lugar do sistema onde um `panic!` e' permitido.
//! Ele e' o mechanismo de `DbC`: pre-condicao devolve `Err` (a entrada e'
//! do mundo externo, o chamador pode estar errado), pos-condicao e
//! invariante
//! quebram o processo (aqui dentro e' bug de programacao, e' bug *nosso*).
//!
//! ## A regra que separa as duas categorias
//!
//! | Categoria | Pergunta que a responde | Como falha |
//! |---|---|---|
//! | **Pre-condicao** | "o chamador podia fazer isso?" | `Err` -- erro e do chamador |
//! | **Pos-condicao** | "a operacao terminou de verdade?" | `panic` -- erro e nosso |
//! | **Invariante** | "o objeto esta coerente?" | `panic` -- erro e nosso |
//!
//! Concusao ironica: em Rust `panic` nao e' "erro nao tratado", e' *panic =
//! abort* sob o perfil de release deste workspace. Isso e' deliberado. Uma
//! pos-condicao furada deve derrubar o processo imediatamente e ser vista no
//! primeiro alerta, nunca virar um `500` ambiguo no meio do trafego.

use std::fmt;

/// Nome estatico do contrato violado. Estatico de proposito: e' endereco de
/// codigo, nao dado, e nao deve alocar em erro.
pub type NomeDoContrato = &'static str;

/// Violacao de pos-condicao ou de invariante.
///
/// Em `debug` e' um `panic!` com rastro legivel; a construcao existe para que
/// o payload possa ser capturado por um `catch_unwind` em teste de contrato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErroDeContrato {
    contrato: NomeDoContrato,
    motivo: String,
}

impl ErroDeContrato {
    /// Cria a violacao. `pub(crate)`: so os macros de contrato instanciam.
    ///
    /// ### Por que `impl Into<String>` e nao `String`
    ///
    /// Aceita `"texto"` e `format!("texto {x}")` com a mesma ergonomicidade.
    /// E' um construtor de erro: a legibilidade do call site vale mais que a
    /// ausencia de uma alocacao que so acontece no caminho de erro.
    pub(crate) fn novo(contrato: NomeDoContrato, motivo: impl Into<String>) -> Self {
        Self {
            contrato,
            motivo: motivo.into(),
        }
    }

    /// Contrato violado. Exposto para auditoria e para relatorio de teste.
    #[must_use]
    pub const fn contrato(&self) -> NomeDoContrato {
        self.contrato
    }

    /// Explicacao legivel da quebra.
    #[must_use]
    pub fn motivo(&self) -> &str {
        &self.motivo
    }
}

impl fmt::Display for ErroDeContrato {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "contrato '{}' violado: {}", self.contrato, self.motivo)
    }
}

impl std::error::Error for ErroDeContrato {}

/// **Pre-condicao.** Avaliada na porta de entrada do objeto.
///
/// Retorna `Err` quando o *chamador* violou o contrato. `motivo` e' aceito
/// como expressao `format!`, e avaliado **apenas** quando a condicao falha --
/// caminho frio nao paga por formatar string.
///
/// ```ignore
/// precondicao!(idade >= 0, "idade nao negativa", "recebido {idade}");
/// ```
#[macro_export]
macro_rules! precondicao {
    ($condicao:expr, $contrato:expr, $($motivo:tt)+) => {
        if !$condicao {
            return Err($crate::contrato::ErroDeContrato::novo(
                $contrato,
                format!($($motivo)+),
            ));
        }
    };
}

/// **Pos-condicao / invariante.** Avaliada *depois* do efeito, dentro do objeto.
///
/// Quebra o processo. Use para prometer algo que o chamador nao pode
/// verificar sozinho -- tipicamente "a escrita no banco afetou 1 linha" ou
/// "o objeto saiu coerente". Jamais para validar entrada do mundo.
#[macro_export]
macro_rules! exigir {
    ($condicao:expr, $contrato:expr, $($motivo:tt)+) => {
        if !$condicao {
            panic!(
                "pos-condicao '{}' furada: {}",
                $contrato,
                format_args!($($motivo)+),
            );
        }
    };
}

/// **Invariante de objeto.** Usada dentro de metodos de transicao, nunca
/// em checagem externa repetida por camada.
///
/// Diferenca de leitura em relacao a [`exigir!`]: a invariante descreve o
/// estado que o objeto mantem *sempre*; a pos-condicao descreve o efeito
/// desta chamada. `panic` nas duas, mas a pergunta e' diferente.
#[macro_export]
macro_rules! invariante {
    ($condicao:expr, $contrato:expr, $($motivo:tt)+) => {
        if !$condicao {
            panic!(
                "invariante '{}' furada: {}",
                $contrato,
                format_args!($($motivo)+),
            );
        }
    };
}