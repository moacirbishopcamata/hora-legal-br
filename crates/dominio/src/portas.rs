//! # Portas: os contratos que o dominio faz com o mundo
//!
//! Uma porta e' uma interface **declarada pelo consumidor** e implementada por
//! quem esta do outro lado. E' o mecanismo concreto da inversao de
//! dependencia (SOLID, `D`).
//!
//! ## Por que a porta vive no dominio, e nao no adaptador
//!
//! Se `FonteDeTempo` estivesse em `hora-legal-ntp`, entao o caso de uso
//! (`hora-legal-api`) dependeria do adaptador -- e a seta da Clean
//! Architecture viraria. Colocando a porta no dominio:
//!
//! ```text
//!   hora-legal-api  --depende de-->  hora-legal-dominio  <--implementa-- hora-legal-ntp
//! ```
//!
//! Os dois adaptsos (entrada e saida) dependem do **mesmo** nucleo, e nao um
//! do outro. Trocar UDP por NTS nao muda uma linha do caso de uso.
//!
//! ## Pre-condicoes e invariantes das implementacoes
//!
//! **Pre-condicoes** (de quem implementa):
//! - `sondar` respeita o prazo que recebe e devolve `TempoEsgotado` ao
//!   estourar, em vez de pendurar;
//! - `decidir` nao altera estado (pos-condicao de leitura).
//!
//! **Invariantes** (de quem usa):
//! - `I1` toda `AmostraDeTempo` devolvida por `sondar` foi validada -- nao
//!   existe "amostra crua" atravessando a porta;
//! - `I2` para toda `d` em `decidir(d)`, existe um `sondar` correspondente que
//!   pode produzir amostra valida. Uma porta que so sabe dizer "nao" seria
//!   inerte.

use std::time::Duration;

use crate::amostra::AmostraDeTempo;
use crate::instante::InstanteUtc;
use crate::resiliencia::DecisaoDeConsulta;

/// Falha de uma sondagem, tipada pelo **motivo** e nao pela mensagem.
///
/// ### Por que o enum e' fechante e nao `String`
///
/// O caso de uso decide o que fazer conforme o tipo: uma fonte fora do ar
/// vale retentativa, um pacote malformado **nao** -- reenviar produz o mesmo
/// pacote. Com `String`, essa distincao morre e o codigo vira
/// `if motivo.contains("timeout")`, que quebra no primeiro erro de DNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeSondagem {
    /// O prazo estourou antes da resposta.
    TempoEsgotado {
        /// Prazo que foi excedido.
        prazo: Duration,
    },
    /// Falha de socket: DNS, rota, socket fechado.
    Rede(String),
    /// O pacote chegou, mas nao pode ser usado.
    PacoteInvalido(String),
    /// A fonte recusou explicitamente (*kiss-of-death*).
    Recusada {
        /// Descricao do beijo da morte.
        motivo: &'static str,
    },
}

impl ErroDeSondagem {
    /// `true` se vale a pena tentar de novo.
    ///
    /// `PacoteInvalido` e' o unico nao-retentavel: a resposta chegou e era
    /// ruim, entao reenviar recebe a mesma resposta ruim.
    #[must_use]
    pub const fn vale_retentativa(&self) -> bool {
        matches!(self, Self::TempoEsgotado { .. } | Self::Rede(_))
    }
}

impl std::fmt::Display for ErroDeSondagem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TempoEsgotado { prazo } => write!(f, "tempo esgotado apos {prazo:?}"),
            Self::Rede(m) => write!(f, "falha de rede: {m}"),
            Self::PacoteInvalido(m) => write!(f, "pacote invalido: {m}"),
            Self::Recusada { motivo } => write!(f, "fonte recusou: {motivo}"),
        }
    }
}

impl std::error::Error for ErroDeSondagem {}

/// Fonte de tempo consultavel.
///
/// ## Assinatura deliberadamente estreita (ISP)
///
/// Tres metodos, e nao um `Repository<FonteDeTempo>`. Cada um diz algo que o
/// consumidor precisa; nada sobra. Um metodo a mais aqui seria metodo a menos
/// que um *mock* precisaria implementar.
pub trait FonteDeTempo: Send + Sync + std::fmt::Debug {
    /// Nome logico, para diagnostico e para a resposta da API.
    fn nome(&self) -> &str;

    /// Endpoint consultado, como `host:porta`.
    ///
    /// **Pre-condicao:** nenhuma -- o endpoint vem do construtor do adaptador.
    ///
    /// ## Por que isto e' do dominio e nao do adaptador
    ///
    /// `/api/v1/fontes` precisa listar **o que foi consultado**, e essa e' a
    /// mesma informacao que o adaptador usou. Se a porta devolvesse so o
    /// nome, a camada HTTP inventaria o endpoint -- e mostraria o nome do
    /// servidor em vez do endereco real, divergindo do que o socket abriu.
    fn endpoint(&self) -> &str;

    /// Decide se a fonte pode ser usada sem tocar a rede.
    ///
    /// **Pos-condicao:** efeito nulo. Quem chama decide o que fazer com a
    /// resposta; esta funcao apenas informa.
    fn decidir(&self, agora: InstanteUtc, janela: Duration) -> DecisaoDeConsulta;

    /// Ultima calibracao **com medicao real**, se houver.
    ///
    /// Serve ao modo degradado: quando nenhuma fonte responde, o caso de uso
    /// ainda pode servir a hora corrigida pela melhor calibracao conhecida,
    /// dentro do limite de idade. **Pos-condicao:** `Some` implica
    /// `tem_medida()`; nunca devolve calibracao de preenchimento.
    fn calibracao_vigente(&self) -> Option<crate::resiliencia::Calibracao> {
        None
    }

    /// Executa uma sondagem de rede.
    ///
    /// ## Por que `Pin<Box<dyn Future>>` e nao `async fn`
    ///
    /// `async fn` numa trait **nao e' `dyn`-compativel**, e esta porta precisa
    /// ser: o caso de uso guarda `Vec<Arc<dyn FonteDeTempo>>` e escolhe a
    /// fonte em runtime. `impl Future` resolveria a assinatura, mas quebra o
    /// `dyn` -- trocariamos inversao de dependencia por monomorfizacao.
    ///
    /// `Pin<Box<dyn Future + Send>>` e' a saida: o `Box` custa uma alocacao
    /// por sondagem, o que e' **irrelevante** ao lado do round-trip de rede que
    /// a segue (dezenas de milissegundos). O `Send` e' obrigatorio para
    /// `tokio::spawn`.
    ///
    /// **Pos-condicao:** em `Ok`, a amostra ja satisfaz todas as
    /// pre-condicoes do dominio.
    fn sondar(
        &self,
        agora: InstanteUtc,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AmostraDeTempo, ErroDeSondagem>> + Send + '_>>;
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn pacote_invalido_nao_e_retentavel() {
        let e = ErroDeSondagem::PacoteInvalido("curto".into());
        assert!(
            !e.vale_retentativa(),
            "reenviar nao conserta resposta malformada"
        );
    }

    #[test]
    fn timeout_e_rede_sao_retentaveis() {
        assert!(ErroDeSondagem::TempoEsgotado {
            prazo: Duration::from_millis(1)
        }
        .vale_retentativa());
        assert!(ErroDeSondagem::Rede("sem rota".into()).vale_retentativa());
        assert!(!ErroDeSondagem::Recusada {
            motivo: "limite-de-taxa"
        }
        .vale_retentativa());
    }

    #[test]
    fn erro_nao_expoe_detalhe_de_socket() {
        // LGPD e seguranca: a mensagem interna nao deve vazar para a API.
        let e = ErroDeSondagem::Rede("connect ETIMEDOUT 10.0.0.1:123".into());
        let msg = e.to_string();
        assert!(msg.contains("falha de rede"), "prefixo estavel: {msg}");
    }
}