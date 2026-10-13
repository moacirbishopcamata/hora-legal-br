//! # Hora Legal Brasileira -- nucleo de dominio (DDD)
//!
//! Camada **sem dependencias externas**: nenhuma crate de I/O, de serializacao
//! ou de data/hora. O `[dependencies]` deste pacote esta vazio, e isso e'
//! verificavel com `cargo tree`. E' o que garante que as regras de negocio
//! rodem em qualquer contexto.
//!
//! ## Os contextos limitados
//!
//! Este nucleo contem **tres** contextos. A separacao nao e' por pastas: e'
//! por **taxa de mudanca** e por **dependencia** -- nenhum contexto importa
//! outro que nao esteja abaixo dele.
//!
//! ```text
//!  MEDIA-CAMADA   medicao  ->  tempo
//!  BAIXA-CAMADA   tempo
//!  SUPORTE        politica, resiliencia, portas   (dependem de tempo; nao o contrario)
//! ```
//!
//! | Contexto | Pergunta que responde | Agregado raiz | Value objects |
//! |---|---|---|---|
//! | **Tempo** | "que instante e' este?" | -- (so VOs) | `InstanteUtc`, `SegundosNtp`, `DataHoraCivil`, `DeslocamentoDeFuso`, `EscalaDoHost`, `ContagemDoHost`, `Formato` |
//! | **Medicao** | "esta leitura e' confiavel?" | `AmostraDeTempo` | `LeiturasBrutas`, `DesvioDeRelogio`, `AtrasoDeRede`, `Estrato`, `BeijoDaMorte` |
//! | **Sincronizacao** | "de onde tiro a hora agora?" | `FonteResiliente` | `AlvoDeConsulta`, `PoliticaDeConsulta`, `Calibracao`, `Disjuntor`, `Espera` |
//!
//! ## Value Objects: a base do design
//!
//! Um value object e' definido por **seus atributos**, nao por identidade:
//! dois `InstanteUtc` com o mesmo valor sao **indistinguiveis** e
//! intercambiaveis. Tres consequencias praticas, todas aplicadas aqui:
//!
//! 1. **Imutabilidade total.** Nao existe `set_*`. Um VO que aceita mutacao
//!    deixa de ser VO e vira entidade. `InstanteUtc`, `DesvioDeRelogio`,
//!    `AtrasoDeRede` e `DataHoraCivil` nao tem como mudar de estado.
//!
//! 2. **Invariante garantida na construcao, nao depois.** O construtor
//!    *total* impede estados invalidos de existirem. `InstanteUtc` so tem
//!    `nanos < 1_000_000_000` porque o unico caminho de construcao valida --
//!    nao ha `Default` publico, nao ha campos publicos.
//!
//! 3. **Igualdade por valor.** `PartialEq`/`Ord` derivados significam que a
//!    invariante e' respeitada por construcao: nao existe dois "iguais" com
//!    conteudos diferentes.
//!
//! ## Entidades e agregados
//!
//! **Agregado = fronteira de consistencia.** Uma so entidade pode ser
//! modificada por fora, e so dentro dela as invariantes sao garantidas.
//!
//! | Agregado raiz | Invariante do agregado | Tudo dentro |
//! |---|---|---|
//! | [`amostra::AmostraDeTempo`] | "`desvio` e `atraso` sao **derivados** de `leituras`, nunca informados" | `LeiturasBrutas`, `DesvioDeRelogio`, `AtrasoDeRede`, `Estrato` |
//! | [`resiliencia::FonteResiliente`] | "uma fonte fora do ar nao gera trafego de sondagem" | `Espera`, `Disjuntor`, `Calibracao` |
//!
//! Um VO **nao tem raiz**: e' identificado pelo valor e nao e' persistido
//! sozinho. `DesvioDeRelogio` nunca e' um repositorio; ele vive dentro da
//! `AmostraDeTempo` e sai junto com ela.
//!
//! ## Pre-condicoes, pos-condicoes e invariantes
//!
//! | Categoria | Onde vive | Como falha |
//! |---|---|---|
//! | **Pre-condicao** | no factory / construtor | `Err` -- a entrada e' de fora |
//! | **Invariante** | dentro do metodo de transicao | `panic!` -- o bug e' nosso |
//! | **Pos-condicao** | logo apos o efeito | `panic!` -- o efeito nao aconteceu |
//!
//! Os tres macros sao [`exigir!`] (pos-condicao e invariante) e
//! [`precondicao!`] (pre-condicao). Todos os contratos do nucleo estao
//! marcados com esses rotulos na documentacao.
//!
//! ## Dependencias
//!
//! ```text
//!   hora-legal-api  (axum)  --depende de-->  hora-legal-dominio
//!                                      ^
//!                                      |
//!   hora-legal-ntp (tokio) ---------------+
//! ```
//!
//! Os dois adaptadores dependem do **mesmo** nucleo e nao um do outro. Trocar
//! UDP por NTS nao muda uma linha do caso de uso.
//!
//! ## Ubiquitous Language
//!
//! Glossario completo em `docs/GLOSSARIO.md`.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod alvo;
pub mod amostra;
pub mod civil;
pub mod contrato;
pub mod formatos;
pub mod fuso;
pub mod host;
pub mod instante;
pub mod politica;
pub mod portas;
pub mod relogio;
pub mod resiliencia;

pub mod fixture;

// ===========================================================================
// Value objects de TEMPO
// ===========================================================================

/// [`InstanteUtc`] -- um ponto na linha do tempo. VO canonico do sistema.
pub use instante::{InstanteUtc, SegundosNtp};
/// [`DataHoraCivil`] -- o mesmo instante em calendario.
pub use civil::DataHoraCivil;
/// [`DeslocamentoDeFuso`] e [`HoraLegalBrasileira`] -- fusos.
pub use fuso::{DeslocamentoDeFuso, HoraLegalBrasileira};
/// Escala e contagem nativas do SO host.
pub use host::{
    ContagemDoHost, EscalaDoHost, FamiliaDeContagem, SistemaDoHost, EPOCH_WINDOWS_EM_SEGUNDOS_UNIX,
};
/// [`Formato`] e [`Formatos`] -- representacoes do mesmo instante.
pub use formatos::{Formatos, Formato};

// ===========================================================================
// Value objects e agregado de MEDICAO
// ===========================================================================

/// [`AmostraDeTempo`] -- **agregado raiz** de Medicao.
///
/// Construtor unico [`amostra::AmostraDeTempo::de_bruta`], que aplica todas
/// as pre-condicoes de uma vez. Nao existe forma de construir uma amostra
/// invalida.
pub use amostra::{
    AtrasoDeRede, AmostraDeTempo, BeijoDaMorte, DesvioDeRelogio, Estrato, LeiturasBrutas,
};

// ===========================================================================
// Entidades de SINCRONIZACAO
// ===========================================================================

/// [`AlvoDeConsulta`] -- entidade: identificada pelo nome logico.
pub use alvo::{alvos_do_ntp_br, AlvoDeConsulta, PORTA_NTP};
/// [`PoliticaDeConsulta`] -- VO validado, com perfis nomeados.
pub use politica::{AjustesDePolitica, Perfil, PoliticaDeConsulta, TETO_DE_TENTATIVAS};
/// [`FonteResiliente`] -- **agregado raiz** de Sincronizacao.
pub use resiliencia::{
    disjuntor_padrao, espera_padrao, Calibracao, DecisaoDeConsulta, Disjuntor, Espera,
    EstadoDoDisjuntor, FonteResiliente,
};

// ===========================================================================
// Suporte transversal
// ===========================================================================

/// [`ErroDeContrato`] -- o tipo unico de violacao de `DbC`.
pub use contrato::ErroDeContrato;
/// Portas de saida: o dominio declara, o adaptador implementa.
pub use portas::{ErroDeSondagem, FonteDeTempo};
/// Portas de tempo e de aleatoriedade, para teste deterministico.
pub use relogio::{Relogio, RelogioDoSistema, RelogioManual, Sorteio, SorteioCiclico, SorteioFixo};