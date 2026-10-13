//! # Erros: do dominio ao HTTP
//!
//! Duas tradicoes, e a separacao entre elas e' o ponto deste modulo:
//!
//! 1. [`ErroDeResposta`] -- **o que** o cliente recebe (status + RFC 9457);
//! 2. [`cabecalho_de_correlacao`] -- **como** o operador correlaciona o log do
//! servidor com a resposta.
//!
//! ## A regra de nao-vazar
//!
//! Nenhuma mensagem de resposta contem IP interno, nome de socket, porta
//! efemera ou rastro de pilha. O `detalhe` diz **o que aconteceu** ("nenhuma
//! fonte respondeu dentro do prazo"), nunca **onde** ("falha em
//! 200.160.0.8:41234"). Um servico de tempo e' alvo natural de varredura, e
//! a topologia das fontes nao deve ser gratuita.
//!
//! O diagnostico completo vai para o log do servidor, com a correlacao
//! partagé -- e o unico lugar onde ele existe.
//!
//! ## Pre-condicoes
//!
//! - `status` coerente com o titulo: `503` nunca vem com "hora invalida";
//! - `correlacao` nunca vazia.

use std::fmt;

use hora_legal_dominio::formatos::Formato;
use hora_legal_dominio::instante::{ErroDeEscrita, InstanteUtc};

use crate::caso_de_uso::ErroDeConsulta;
use crate::dto::ProblemaDeResposta;

/// Base dos URIs de tipo do problema.
///
/// Um URI, nao uma string solta: e' o que permite ao cliente *buscar* a
/// documentacao do tipo de erro, e o que RFC 9457 exige.
const BASE_DE_TIPOS: &str = "https://hora-legal.br/problemas/";

/// Erro que a camada HTTP sabe responder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeResposta {
    /// Parametro de consulta invalido (`?perfis=xxx`, `?formatos=yyy`).
    ///
    /// `400`: o cliente errou, e reenviar igual vai falhar igual.
    ParametroInvalido {
        /// Nome do parametro.
        parametro: &'static str,
        /// Valor recebido.
        recebido: String,
        /// Valores aceitos, para a mensagem ser acionavel.
        aceitos: &'static str,
    },

    /// Formato pedido existe, mas o instante nao cabe no epoch dele.
    ///
    /// `422`: o pedido e' sintaticamente valido, mas semanticamente impossivel.
    FormatoForaDoEpoch {
        /// Formato pedido.
        formato: Formato,
    },

    /// Nenhuma fonte respondeu.
    ///
    /// `503` + `Retry-After`: o cliente deve tentar de novo, e o servidor
    /// diz quando.
    FonteIndisponivel {
        /// Quantas fontes foram tentadas.
        fontes: usize,
        /// Sintoma agregado, sem detalhe de socket.
        sintoma: String,
        /// Segundos sugeridos antes de repetir.
        repetir_em_s: u64,
    },

    /// A calibracao existente e' velha demais para ser servida.
    ///
    /// `503`: servir uma hora de horas atras seria pior que falhar.
    CalibracaoVelha {
        /// Idade da calibracao, em segundos.
        idade_s: u64,
        /// Limite ultrapassado, em segundos.
        limite_s: u64,
    },
}

impl ErroDeResposta {
    /// Tipo URI do problema.
    #[must_use]
    pub const fn tipo_uri(&self) -> &'static str {
        match self {
            Self::ParametroInvalido { .. } => "parametro-invalido",
            Self::FormatoForaDoEpoch { .. } => "formato-fora-do-epoch",
            Self::FonteIndisponivel { .. } => "fonte-indisponivel",
            Self::CalibracaoVelha { .. } => "calibracao-velha",
        }
    }

    /// Titulo curto e estavel.
    #[must_use]
    pub const fn titulo(&self) -> &'static str {
        match self {
            Self::ParametroInvalido { .. } => "Parametro invalido",
            Self::FormatoForaDoEpoch { .. } => "Formato nao representa este instante",
            Self::FonteIndisponivel { .. } => "Nenhuma fonte de tempo disponivel",
            Self::CalibracaoVelha { .. } => "Calibracao de tempo vencida",
        }
    }

    /// Codigo HTTP.
    ///
    /// **Pre-condicao:** coerente com [`Self::titulo`]. Um `503` com titulo
    /// de erro de cliente faria o cliente se behaviourar errado.
    #[must_use]
    pub const fn status(&self) -> u16 {
        match self {
            Self::ParametroInvalido { .. } => 400,
            // 422: sintaxe ok, semantica impossivel. Diferente de 400 porque
            // o cliente nao ganha nada em "corrigir" o parametro -- ele pediu
            // algo legitimo que o servidor nao consegue representar.
            Self::FormatoForaDoEpoch { .. } => 422,
            Self::FonteIndisponivel { .. } | Self::CalibracaoVelha { .. } => 503,
        }
    }

    /// Detalhe, **sem vazar infraestrutura**.
    #[must_use]
    pub fn detalhe(&self) -> String {
        match self {
            Self::ParametroInvalido {
                parametro,
                recebido,
                aceitos,
            } => format!("`{parametro}` recebeu `{recebido}`; aceita: {aceitos}"),
            Self::FormatoForaDoEpoch { formato } => format!(
                "`{}` conta desde o epoch proprio, que nao alcanca este instante",
                formato.nome()
            ),
            Self::FonteIndisponivel { fontes, sintoma, .. } => {
                format!("{fontes} fontes consultadas sem sucesso; ultimo sintoma: {sintoma}")
            }
            Self::CalibracaoVelha { idade_s, limite_s } => {
                format!("calibracao com {idade_s}s excede o limite de {limite_s}s")
            }
        }
    }

    /// Segundos de `Retry-After`, quando faz sentido.
    ///
    /// `400` e `422` **nao** tem `Retry-After`: reenviar o mesmo pedido
    /// falhar igual, e um cabecalho de espera so faria o cliente esperar.
    #[must_use]
    pub const fn repetir_em_s(&self) -> Option<u64> {
        match self {
            Self::ParametroInvalido { .. } | Self::FormatoForaDoEpoch { .. } => None,
            Self::FonteIndisponivel { repetir_em_s, .. } => Some(*repetir_em_s),
            Self::CalibracaoVelha { .. } => Some(1),
        }
    }

    /// Converte no corpo RFC 9457.
    #[must_use]
    pub fn para_problema(&self, correlacao: &str) -> ProblemaDeResposta {
        ProblemaDeResposta::novo(
            Self::uri_de(self.tipo_uri()),
            self.titulo(),
            self.status(),
            self.detalhe(),
            correlacao,
        )
    }

    /// URI completo do tipo de problema.
    ///
    /// ## Por que uma tabela `const` e nao `Box::leak`
    ///
    /// `Box::leak` resolveria, mas vazaria memoria de proposito para
    /// "economizar" uma alocacao por requisicao -- o preco escondido de um
    /// `unwrap`, em outra forma. Uma tabela `const` de `Cow` resolve **sem
    /// vazamento e sem alocacao em runtime**: o texto esta no binario, e
    /// `Cow::Borrowed` devolve `&'static str`.
    ///
    /// **Invariante:** todo `tipo_uri()` tem exatamente uma entrada na
    /// tabela. O teste `todo_uri_tem_entrada_na_tabela` verifica.
    #[must_use]
    fn uri_de(nome: &str) -> std::borrow::Cow<'static, str> {
        const URIS: [std::borrow::Cow<'static, str>; 4] = [
            std::borrow::Cow::Borrowed(
                "https://hora-legal.br/problemas/parametro-invalido",
            ),
            std::borrow::Cow::Borrowed(
                "https://hora-legal.br/problemas/formato-fora-do-epoch",
            ),
            std::borrow::Cow::Borrowed("https://hora-legal.br/problemas/fonte-indisponivel"),
            std::borrow::Cow::Borrowed("https://hora-legal.br/problemas/calibracao-velha"),
        ];
        URIS
            .iter()
            .find(|u| u.ends_with(nome))
            .cloned()
            .unwrap_or(std::borrow::Cow::Borrowed(BASE_DE_TIPOS))
    }
}

impl fmt::Display for ErroDeResposta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.titulo(), self.detalhe())
    }
}

impl std::error::Error for ErroDeResposta {}

/// Traduz um erro de **dominio** para o erro HTTP.
///
/// ## Por que a traducao e' explicita e nao um `From` implicito
///
/// Um `From` esconderia **qual** erro de dominio vira **qual** status. Com a
/// funcao nomeada, a tabela de traducao e' uma lista -- e o teste
/// `traducoes_de_erro_estao_completas` verifica que nenhuma variante ficou de
/// fora. Uma `From` forgotten e' um `500` silencioso em producao.
///
/// **Pre-condicao:** a politica vigente e' valida, entao a leitura de
/// `janela_de_coerencia` nunca falha.
#[must_use]
pub fn traduzir_erro_de_consulta(
    erro: &ErroDeConsulta,
    repetir_em_s: u64,
) -> ErroDeResposta {
    match erro {
        ErroDeConsulta::NenhumaFonteRespondeu { fontes, ultimo } => {
            ErroDeResposta::FonteIndisponivel {
                fontes: *fontes,
                // O sintoma vem do adaptador e pode conter nome de host. Aqui
                // ele e' reduzido a uma categoria: a topologia das fontes nao
                // e' publica.
                sintoma: categoria_do_sintoma(ultimo),
                repetir_em_s,
            }
        }
        ErroDeConsulta::CalibracaoVelha { idade, limite } => ErroDeResposta::CalibracaoVelha {
            idade_s: u64::try_from(idade.as_secs()).unwrap_or(u64::MAX),
            limite_s: u64::try_from(limite.as_secs()).unwrap_or(u64::MAX),
        },
        ErroDeConsulta::ServindoEmDegradacao { .. } => {
            // Este caso **nao** vira erro: e' resposta com `degradado`. Se
            // chegar aqui, e' bug de fluxo -- e o tratamento honesto e'
            // recusar com 503, nao servir hora nenhuma.
            ErroDeResposta::FonteIndisponivel {
                fontes: 0,
                sintoma: "calibracao servida em degradacao".to_owned(),
                repetir_em_s,
            }
        }
    }
}

/// Traduz um erro de **escrita** para o erro HTTP.
///
/// **Pre-condicao:** o erro veio de `Formato::escrever`.
#[must_use]
pub fn traduzir_erro_de_escrita(formato: Formato, erro: &ErroDeEscrita) -> ErroDeResposta {
    match erro {
        // `DestinoRecusou` e' falha **interna** (buffer cheio, writer
        // quebrado). Nao e' culpa do cliente e nao vira `4xx`: vira `500`
        // com o sintoma generico, e o log do servidor carrega o detalhe.
        ErroDeEscrita::DestinoRecusou => ErroDeResposta::FonteIndisponivel {
            fontes: 0,
            sintoma: "falha interna ao montar a resposta".to_owned(),
            repetir_em_s: 1,
        },
        ErroDeEscrita::InstanteForaDoEpoch(_) => {
            ErroDeResposta::FormatoForaDoEpoch { formato }
        }
    }
}

/// Reduz um sintoma tecnico a uma categoria publica.
///
/// ## Por que esta funcao existe
///
/// `ErroDeSondagem::Rede("connect ETIMEDOUT 200.160.0.8:41234")` e' util
/// para o log do servidor e **prejudicial** para a resposta: revela a topologia
/// das fontes a qualquer scanner. Aqui o sintoma vira "tempo esgotado" ou
/// "falha de rede" -- informacao suficiente para o cliente decidir se insiste,
/// insuficiente para mapear a rede do ON.
#[must_use]
fn categoria_do_sintoma(sintoma: &str) -> String {
    // Classificacao por marcador textual e' feia; classificar por *tipo* seria
    // melhor, mas o tipo nao atravessa a fronteira de dominio. A solucao
    // correta seria tipar o sintoma no adaptador -- e isso esta anotado em
    // `docs/TODO.md`. Por enquanto, o mapeamento e' conservador: em caso
    // duvidoso, devolve "falha desconhecida" em vez de repassar o texto.
    let marcas: [(&str, &str); 4] = [
        ("esgotado", "tempo esgotado"),
        ("timeout", "tempo esgotado"),
        ("rede", "falha de rede"),
        ("recusou", "fonte recusou"),
    ];
    for (marca, categoria) in marcas {
        if sintoma.contains(marca) {
            return String::from(categoria);
        }
    }
    String::from("falha desconhecida")
}

/// Cabecalho de correlacao da resposta.
///
/// **Contrato:** presente em **toda** resposta, inclusive de sucesso. E' o
/// que permite casar `curl -i` com o log do servidor.
#[must_use]
pub fn cabecalho_de_correlacao(correlacao: &str) -> (&'static str, String) {
    ("x-hora-legal-correlacao", correlacao.to_owned())
}

/// Gera um identificador de correlacao.
///
/// Sem dependencia externa: um contador atomico combinado com o instante
/// corrente. **Nao e' criptografico e nao precisa ser** -- e' correlacao de
/// log, nao token de autenticacao. Um UUID exigiria uma crate inteira para
/// resolver um problema que o contador resolve.
#[must_use]
pub fn gerar_correlacao(instante: InstanteUtc) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCIA: AtomicU64 = AtomicU64::new(0);
    let n = SEQUENCIA.fetch_add(1, Ordering::Relaxed);
    // Milissegundos + sequencia: ordenavel por tempo **e** unico dentro do
    // processo. Um so sem o outro falharia em requisicoes simultaneas.
    format!("{:x}-{:04x}", instante.milissegundos_desde_epoch(), n & 0xffff)
}

#[cfg(test)]
mod testes {
    use super::*;
    use hora_legal_dominio::fixture::instante;
    use std::time::Duration;

    #[test]
    fn parametros_invalidos_sao_400_sem_retry_after() {
        let e = ErroDeResposta::ParametroInvalido {
            parametro: "perfis",
            recebido: "xxx".into(),
            aceitos: "compacto, completo, numerico",
        };
        assert_eq!(e.status(), 400);
        assert!(
            e.repetir_em_s().is_none(),
            "reenviar o mesmo pedido falharia igual; Retry-After nao ajuda"
        );
    }

    #[test]
    fn formato_fora_do_epoch_e_422_nao_400() {
        // 422 e' o codigo certo: o parametro e' valido, a **semantica** nao e'.
        let e = ErroDeResposta::FormatoForaDoEpoch {
            formato: Formato::WindowsFileTime,
        };
        assert_eq!(e.status(), 422);
        assert!(e.repetir_em_s().is_none());
    }

    #[test]
    fn fonte_indisponivel_e_503_com_retry_after() {
        let e = ErroDeResposta::FonteIndisponivel {
            fontes: 6,
            sintoma: "tempo esgotado".into(),
            repetir_em_s: 2,
        };
        assert_eq!(e.status(), 503);
        assert_eq!(e.repetir_em_s(), Some(2));
    }

    #[test]
    fn sintoma_nao_vaza_topologia_das_fontes() {
        // Este e' o teste de seguranca. O sintoma bruto traz IP e porta
        // efemera; a resposta nao pode conter nenhum dos dois.
        let erro = ErroDeConsulta::NenhumaFonteRespondeu {
            fontes: 6,
            ultimo: "falha de rede: connect ETIMEDOUT 200.160.0.8:41234".into(),
        };
        let http = traduzir_erro_de_consulta(&erro, 1);
        let detalhe = http.detalhe();
        assert!(!detalhe.contains("200.160.0.8"), "IP vazou: {detalhe}");
        assert!(!detalhe.contains("41234"), "porta vazou: {detalhe}");
        assert!(!detalhe.contains("ETIMEDOUT"), "errno vazou: {detalhe}");
        assert!(
            detalhe.contains("tempo esgotado") || detalhe.contains("rede"),
            "a categoria continua util: {detalhe}"
        );
    }

    #[test]
    // `allow` local: um teste que passasse em silencio quando o sintoma
    // *vazasse* seria o pior resultado possivel -- e `panic!` aqui e' o
    // mecanismo que garante que ele nao passa.
    #[allow(clippy::panic)]
    fn sintoma_desconhecido_vira_categoria_e_nao_texto() {
        // Falha de bloqueio: nao repassa o texto bruto quando nao reconhece.
        let erro = ErroDeConsulta::NenhumaFonteRespondeu {
            fontes: 1,
            ultimo: "kernel panic in thread 'x' at /home/user/segredo.rs:42".into(),
        };
        let detalhe = traduzir_erro_de_consulta(&erro, 1).detalhe();
        assert!(!detalhe.contains("segredo"), "caminho vazou: {detalhe}");
        assert!(detalhe.contains("desconhecida"), "{detalhe}");
    }

    #[test]
    fn traducoes_de_erro_estao_completas() {
        // Cada variante de `ErroDeConsulta` produz um `ErroDeResposta` com
        // status coerente. Uma variante nova sem traducao daria `500`.
        let casos = [
            ErroDeConsulta::NenhumaFonteRespondeu {
                fontes: 1,
                ultimo: "x".into(),
            },
            ErroDeConsulta::CalibracaoVelha {
                idade: Duration::from_secs(9),
                limite: Duration::from_secs(1),
            },
            ErroDeConsulta::ServindoEmDegradacao {
                idade: Duration::from_secs(9),
            },
        ];
        for caso in casos {
            let http = traduzir_erro_de_consulta(&caso, 1);
            assert!(http.status() >= 400, "{caso:?} virou {http:?}");
            assert!(!http.detalhe().is_empty(), "detalhe vazio para {caso:?}");
        }
    }

    #[test]
    fn erro_de_escrita_interno_nao_vira_4xx() {
        // DestinoRecusou e' falha nossa, nao do cliente.
        let e = traduzir_erro_de_escrita(Formato::Rfc3339, &ErroDeEscrita::DestinoRecusou);
        assert_eq!(e.status(), 503, "falha interna nao pode parecer erro de cliente");
    }

    #[test]
    fn correlacao_e_unica_e_ordenavel() {
        let i = instante(2026, 10, 9, 17, 0, 0, 0);
        let a = gerar_correlacao(i);
        let b = gerar_correlacao(i);
        assert_ne!(a, b, "duas requisicoes no mesmo ms precisam de ids distintos");
        assert!(a.starts_with(&format!("{:x}", i.milissegundos_desde_epoch())));
        assert_eq!(gerar_correlacao(i).split('-').next(), a.split('-').next());
    }

    #[test]
    fn todo_uri_tem_entrada_na_tabela() {
        // Invariante da tabela: um `tipo_uri()` sem entrada cairia no
        // `BASE_DE_TIPOS` silenciosamente, e o cliente receberia um URI que
        // nao descreve o erro.
        let tipos: [ErroDeResposta; 4] = [
            ErroDeResposta::ParametroInvalido {
                parametro: "p",
                recebido: "v".into(),
                aceitos: "a",
            },
            ErroDeResposta::FormatoForaDoEpoch {
                formato: Formato::NtpSegundos,
            },
            ErroDeResposta::FonteIndisponivel {
                fontes: 0,
                sintoma: "s".into(),
                repetir_em_s: 1,
            },
            ErroDeResposta::CalibracaoVelha {
                idade_s: 1,
                limite_s: 0,
            },
        ];
        for e in &tipos {
            let uri = ErroDeResposta::uri_de(e.tipo_uri());
            assert!(
                uri.ends_with(e.tipo_uri()),
                "URI {:?} nao termina com {:?}; caiu no fallback",
                uri,
                e.tipo_uri()
            );
        }
    }

    #[test]
    fn todo_problema_tem_uri_absoluto() {
        for e in [
            ErroDeResposta::ParametroInvalido {
                parametro: "p",
                recebido: "v".into(),
                aceitos: "a",
            },
            ErroDeResposta::FormatoForaDoEpoch {
                formato: Formato::NtpSegundos,
            },
            ErroDeResposta::FonteIndisponivel {
                fontes: 0,
                sintoma: "s".into(),
                repetir_em_s: 1,
            },
            ErroDeResposta::CalibracaoVelha {
                idade_s: 1,
                limite_s: 0,
            },
        ] {
            let p = e.para_problema("abc");
            assert!(
                p.tipo.starts_with("https://"),
                "RFC 9457 exige URI absoluto: {}",
                p.tipo
            );
            assert_eq!(p.status, e.status());
            assert!(!p.correlacao.is_empty());
        }
    }
}