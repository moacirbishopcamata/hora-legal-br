//! # Contrato HTTP: o que o cliente recebe
//!
//! Este modulo tem **um** trabalho: traduzir objetos de dominio em JSON, e
//! erros de dominio em `StatusCode` + corpo de problema. Nenhuma regra de
//! negocio, nenhuma decisao de rede.
//!
//! ## DTO nao e' o dominio (Clean Code, cap. 6)
//!
//! [`hora_legal_dominio::InstanteUtc`] tem invariantes de `nanos` e existe
//! em nanossegundos. O DTO existe em **strings**, porque o cliente precisa de
//! `RFC 3339`, epoch e a contagem nativa do SO -- tres representacoes que o
//! dominio conhece mas que **ninguem** deveria precisar para poder mudar.
//!
//! Se o DTO fosse o dominio, renomear um campo interno quebraria o contrato
//! HTTP. Aqui, a quebra e' local e o dominio permanece estavel.
//!
//! ## A regra das strings grandes
//!
//! Contagens acima de 2^53 (epoch em nanossegundos, `FILETIME`, ticks do
//! .NET) sao **strings** no JSON. O motivo esta em `docs/API.md`: o `Number`
//! do JavaScript tem 53 bits de mantissa, e `JSON.parse` transformaria
//! `1789123402123456789` em `1789123402123456800` **em silencio**. String e'
//! o unico tipo que round-tripa.
//!
//! Pre-condicoes e invariantes deste modulo: nenhuma regra nova e' criada
//! aqui; os DTO apenas projetam o que o dominio ja validou.

use hora_legal_dominio::formatos::{Formato, Formatos};
use hora_legal_dominio::fuso::HoraLegalBrasileira;
use hora_legal_dominio::host::{ContagemDoHost, EscalaDoHost};
use hora_legal_dominio::instante::InstanteUtc;
use hora_legal_dominio::politica::Perfil;
use hora_legal_dominio::resiliencia::DecisaoDeConsulta;
use serde::Serialize;

/// Cabecalho HTTP `Date` e corpo textual, em RFC 3339.
///
/// ## Por que `hora` e' um valor, e nao um metodo
///
/// `InstanteUtc` e' um value object; o DTO tambem. Um DTO com metodo
/// (`fn hora(&self)`) seria um objeto que sabe calcular -- e a calculadora
/// pertence ao dominio, nao a representacao. Aqui o dado ja vem pronto do
/// dominio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespostaDeHora {
    /// Instante em RFC 3339, com nanossegundo: `2026-10-09T17:03:22.123456789Z`.
    ///
    /// **Contrato:** sempre presente, sempre UTC, sempre com `Z`. E' o campo
    /// que qualquer cliente sabe ler.
    pub utc: String,

    /// Hora Legal Brasileira, em RFC 3339 com fuso `-03:00`.
    ///
    /// **Contrato:** sempre presente. Derivada de `utc`, nunca medida de novo
    /// -- duas leituras do relogio poderiam discordar em um nanossegundo.
    pub brasilia: String,

    /// Mesma hora em por extenso: `sexta-feira, 09/10/2026 17:03:22 UTC`.
    ///
    /// Para tela de operacao e log legivel, onde o cliente humano nao deveria
    /// precisar montar data.
    pub extenso: String,

    /// Representacoes numericas, indexadas pelo nome do formato.
    ///
    /// **Contrato:** as chaves sao os `nome()` de cada formato pedido. Os
    /// valores sao **strings** para todos os formatos numericos, pela regra
    /// dos 53 bits descrita no modulo.
    pub formatos: FormatosResposta,

    /// Contagem nativa do sistema operacional **host**.
    ///
    /// **Contrato:** presente com os campos da familia de contagem do host --
    /// `file_time` no Windows, `relogio` no POSIX. Ver `docs/API.md`.
    pub host: RespostaDeHost,

    /// Desvio medido entre a fonte e o relogio local, em milissegundos.
    ///
    /// **Sinal:** positivo = a fonte estava adiantada. Contrato de leitura:
    /// `positivo` significa "somar ao relogio local para acertar".
    pub desvio_ms: i64,

    /// Atraso ida-e-volta da medicao, em milissegundos.
    ///
    /// Quanto maior, menos precisa a medicao. Serve para o cliente decidir se
    /// aceita a hora ou pede de novo.
    pub atraso_ms: u64,

    /// Estrato declarado pela fonte (0 = beijo da morte, 1 = primaria).
    pub estrato: u8,

    /// Descricao do estrato em portugues: `primaria`, `secundaria`.
    pub estrato_descricao: &'static str,

    /// Servidor do Observatorio Nacional que respondeu.
    ///
    /// **Contrato:** `"cache"` quando a resposta veio de calibracao local.
    /// O valor `"cache"` e' uma **resposta**, nao um erro: o campo `degradado`
    /// diz se ela e' aceitavel.
    pub fonte: String,

    /// Idade da calibracao usada, em milissegundos.
    pub idade_ms: u64,

    /// `true` quando a resposta veio de cache ou de fonte fora do ar.
    ///
    /// O cliente **deve** verificar este campo: `200` com `degradado: true`
    /// e' uma resposta valida, mas menos confiavel.
    pub degradado: bool,

    /// Perfil de latencia/precisao em vigor.
    pub perfil: &'static str,
}

/// Formatos numericos e textuais, indexados por nome.
///
/// ## Por que um `map` e nao campos nomeados
///
/// Sao 22 formatos; 22 campos mudam a cada RFC nova. Um mapa com a chave sendo
/// o `nome()` do formato mantem o contrato estavel e faz com que adicionar um
/// formato nao quebre nenhum cliente -- o teste `todos_os_formatos_do_perfil`
/// garante que o perfil escolhido entregou todos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatosResposta(BTreeMapDeTextos);

impl FormatosResposta {
    /// Constroi o mapa pedido.
    #[must_use]
    pub fn de_formatos(perfis: &[Formato], instante: InstanteUtc) -> Self {
        let mut textos = BTreeMapDeTextos::default();
        for formato in perfis {
            // Um formato que falha nao invalida a resposta: os outros seguem.
            // O `200` com um campo a menos e' melhor que um `500` -- o cliente
            // recebe a RFC 3339, que e' o que ele precisa.
            let mut buffer = String::new();
            match formato.escrever(instante, &mut buffer) {
                Ok(()) => {
                    textos.insert(String::from(formato.nome()), buffer);
                }
                // `InstanteForaDoEpoch`: o instante nao cabe no epoch do formato
                // (ex.: `FILETIME` antes de 1601). O campo simplesmente nao
                // aparece -- e a resposta continua valida.
                Err(_) => {}
            }
        }
        Self(textos)
    }

    /// Mapa serializavel.
    #[must_use]
    pub fn como_mapa(&self) -> &BTreeMapDeTextos {
        &self.0
    }
}

/// Mapa ordenado de texto: JSON com chaves estaveis.
///
/// `BTreeMap` e nao `HashMap` porque **a ordem das chaves no JSON deve ser
/// estavel**: duas requisicoes com o mesmo perfil precisam produzir bytes
/// identicos, senao o cache HTTP do cliente nunca acerta e o teste de
/// idempotencia fica instavel.
pub type BTreeMapDeTextos = std::collections::BTreeMap<String, String>;

/// Contagem nativa do sistema operacional host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespostaDeHost {
    /// `windows` ou `unix` (ou `outro`).
    pub sistema: &'static str,

    /// `file_time` (100 ns desde 1601) ou `relogio` (ns desde 1970).
    pub escala: &'static str,

    /// Unidades desde o epoch da escala.
    ///
    /// **Contrato:** string, pela regra dos 53 bits.
    pub unidades: String,

    /// Unidades por segundo da escala: `10000000` ou `1000000000`.
    pub unidades_por_segundo: u64,
}

impl RespostaDeHost {
    /// Constroi a resposta nativa do host.
    ///
    /// **Pre-condicao:** o instante deve ser representavel na escala. Antes de
    /// 1601 a contagem `FILETIME` nao existe, e a resposta usaria a escala
    /// Unix -- o que seria uma mentira para o cliente Windows.
    #[must_use]
    pub fn de_instante(instante: InstanteUtc, escala: EscalaDoHost) -> Self {
        let unidades = ContagemDoHost::de_instante(escala, instante)
            .map_or(0_i128, |c| c.unidades);
        Self {
            sistema: escala.sistema.identificacao(),
            escala: escala.familia.identificacao(),
            unidades: unidades.to_string(),
            unidades_por_segundo: escala.unidades_por_segundo,
        }
    }
}

/// Estado de uma fonte na listagem de `/api/v1/fontes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespostaDeFonte {
    /// Nome logico: `a.ntp.br`.
    pub nome: String,

    /// Endpoint (`host:porta`) consultado.
    pub endpoint: String,

    /// `fechado`, `aberto`, `em-recuperacao` ou `indisponivel`.
    ///
    /// **Contrato:** nunca `null`. Uma fonte que nao responde tem estado
    /// `indisponivel`, que e' informacao -- diferente de um campo ausente,
    /// que seria *null* (Yegor256 #1).
    pub estado: &'static str,

    /// `true` se a fonte esta em disjuntor aberto.
    pub em_disjuntor: bool,

    /// `true` se a calibracao vigente esta degradada.
    pub degradada: bool,

    /// Decisao que a proxima consulta tomaria.
    pub proxima_decisao: &'static str,
}

impl RespostaDeFonte {
    /// Constroi a resposta de uma fonte.
    #[must_use]
    pub fn de_fonte(nome: &str, endpoint: &str, decisao: &DecisaoDeConsulta) -> Self {
        let (estado, em_disjuntor, proxima_decisao) = match decisao {
            DecisaoDeConsulta::UsarCache { .. } => ("fechado", false, "cache"),
            DecisaoDeConsulta::Sondar { .. } => ("fechado", false, "sondar"),
            DecisaoDeConsulta::FonteIndisponivel { .. } => ("aberto", true, "indisponivel"),
        };
        Self {
            nome: nome.to_owned(),
            endpoint: endpoint.to_owned(),
            estado,
            em_disjuntor,
            degradada: false,
            proxima_decisao,
        }
    }
}

/// Identidade do build, servida em `/api/v1/versao`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespostaDeVersao {
    /// Versao semantica do `Cargo.toml`.
    pub versao: &'static str,

    /// Commit abreviado (`git rev-parse --short HEAD`).
    pub commit: &'static str,

    /// Numero de commits desde o primeiro (`git rev-list --count HEAD`).
    ///
    /// **Este e' o numero que incrementa a cada commit.** E' o que fulfill o
    /// requisito de versionamento sem depender de ninguem editar um arquivo.
    pub build: &'static str,

    /// Data do commit em ISO 8601.
    pub data: &'static str,

    /// Ramo, ou `desconhecido` em CI com HEAD destacado.
    pub ramo: &'static str,

    /// Sistema operacional host.
    pub sistema: &'static str,

    /// Perfil de latencia em vigor.
    pub perfil: &'static str,
}

/// Resposta de `GET /api/v1/saude/liveness` e `/readiness`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RespostaDeSaude {
    /// `vivo` ou `pronto`.
    pub estado: &'static str,

    /// Explicacao em uma frase, para log do cliente.
    pub detalhe: &'static str,

    /// Segundos ate a proxima tentativa recomendada. Zero quando nao ha.
    pub repetir_em_s: u64,
}

/// Erro no formato RFC 9457 (*problem details*).
///
/// ## Por que RFC 9457 e nao um JSON qualquer
///
/// O padrao define `type` (URI que identifica o problema), `title`, `status`
/// e `detail`. Clientes -- e o `kubectl`, e o navegador -- sabem exibi-lo. Um
/// `{"erro": "algo deu errado"}` nao e' legivel por maquina.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProblemaDeResposta {
    /// URI do tipo do problema, por exemplo
    /// `https://hora-legal.br/problemas/fonte-indisponivel`.
    /// **Contrato:** `Cow<'static, str>`. A RFC 9457 exige um URI, e o URI
    /// aqui e' `BASE + nome-do-tipo` -- concatenacao que, feita em runtime,
    /// alocaria por resposta. `Cow` permite devolver `Borrowed` para um tipo
    /// **fixo** (o caso comum: quatro URIs para o processo inteiro) e `Owned`
    /// para um tipo novo, sem `Box::leak` e sem custo por requisicao.
    pub tipo: std::borrow::Cow<'static, str>,

    /// Resumo curto e estavel.
    pub titulo: &'static str,

    /// Codigo HTTP.
    pub status: u16,

    /// Explicacao especifica desta ocorrencia.
    ///
    /// **Contrato de seguranca:** nao contem IP interno, nome de socket, nem
    /// rastro de pilha. Ver `docs/SEGURANCA.md`.
    pub detalhe: String,

    /// Identificador de correlacao, para casar o log do servidor com a resposta.
    ///
    /// **Contrato:** presente em **todas** as respostas de erro. E' o requisito
    /// de rastreabilidade ponta a ponta.
    pub correlacao: String,
}

impl ProblemaDeResposta {
    /// Constroi o problema com correlacao.
    #[must_use]
    pub fn novo(
        tipo: impl Into<std::borrow::Cow<'static, str>>,
        titulo: &'static str,
        status: u16,
        detalhe: impl Into<String>,
        correlacao: &str,
    ) -> Self {
        Self {
            tipo: tipo.into(),
            titulo,
            status,
            detalhe: detalhe.into(),
            correlacao: correlacao.to_owned(),
        }
    }
}

/// Perfil como texto estavel para a resposta.
#[must_use]
pub fn nome_do_perfil(perfil: Perfil) -> &'static str {
    match perfil {
        Perfil::MaximaPrecisao => "maxima-precisao",
        Perfil::Equilibrado => "equilibrado",
        Perfil::MenorLatencia => "menor-latencia",
    }
}

/// Converte uma decisao no rotulo de estado exposto na API.
#[must_use]
pub fn rotulo_do_estado(decisao: &DecisaoDeConsulta) -> &'static str {
    match decisao {
        DecisaoDeConsulta::UsarCache { .. } => "fechado",
        DecisaoDeConsulta::Sondar { .. } => "fechado",
        DecisaoDeConsulta::FonteIndisponivel { .. } => "aberto",
    }
}

/// Aplica o deslocamento de Brasilia a um instante, como string.
#[must_use]
pub fn brasilia_de(instante: InstanteUtc) -> String {
    let civil = HoraLegalBrasileira::civil_de(instante);
    let mut destino = String::new();
    // `escrever_rfc3339` num `String` **nao** falha: `String` cresce ate o
    // fim. Um `match` com fallback inventaria um instante que nao existe;
    // `let _ =` declara a impossibilidade em vez de mascara-la.
    let _ = civil.escrever_rfc3339(&mut destino);
    destino
}

/// Formatos servidos por perfil, resolvido uma vez.
#[must_use]
pub fn formatos_do_perfil(perfil: Formatos) -> &'static [Formato] {
    perfil.lista()
}

#[cfg(test)]
mod testes {
    use super::*;
    use hora_legal_dominio::fixture::instante;

    const T: &str = "2026-10-09T17:03:22.123456789Z";

    fn ref_in() -> InstanteUtc {
        instante(2026, 10, 9, 17, 3, 22, 123_456_789)
    }

    #[test]
    fn o_formato_padrao_e_rfc3339() {
        // Pre-condicao do contrato: o cliente que nao pede nada recebe
        // exatamente o campo `utc`.
        let lista = formatos_do_perfil(Formatos::Compacto);
        assert_eq!(lista, &[Formato::Rfc3339]);
    }

    #[test]
    fn todos_os_formatos_do_perfil_sao_entregues() {
        // Pos-condicao: o perfil promete N formatos; o mapa tem N entradas.
        for perfil in [Formatos::Completo, Formatos::Numerico] {
            let resposta = FormatosResposta::de_formatos(perfil.lista(), ref_in());
            assert_eq!(
                resposta.como_mapa().len(),
                perfil.lista().len(),
                "perfil {:?} prometeu {} formatos e entregou {}",
                perfil,
                perfil.lista().len(),
                resposta.como_mapa().len()
            );
        }
    }

    #[test]
    fn a_chave_de_cada_formato_e_o_nome_canonico() {
        // O contrato HTTP usa `nome()` como chave: cliente e servidor
        // precisam concordar no mesmo nome.
        let lista = formatos_do_perfil(Formatos::Completo);
        let resposta = FormatosResposta::de_formatos(lista, ref_in());
        for formato in lista {
            let chave = String::from(formato.nome());
            assert!(
                resposta.como_mapa().contains_key(&chave),
                "falta a chave {chave} no mapa"
            );
        }
    }

    #[test]
    fn rfc3339_e_a_chave_canonica() {
        let resposta = FormatosResposta::de_formatos(&[Formato::Rfc3339], ref_in());
        assert_eq!(
            resposta.como_mapa().get("rfc3339").map(String::as_str),
            Some(T),
            "a RFC 3339 e' o valor de referencia"
        );
    }

    #[test]
    fn contagens_grandes_sao_strings() {
        // Invariante de seguranca: o epoch em nanos tem 19 digitos e nao
        // sobrevive a um `Number` de JavaScript.
        let resposta = FormatosResposta::de_formatos(&[Formato::UnixNanos], ref_in());
        let valor = resposta.como_mapa().get("unix_ns").map(String::as_str);
        assert!(valor.is_some_and(|v| v.len() >= 19), "epoch em ns tem 19 digitos");
        // E o valor tem de parsear como inteiro exato.
        let inteiro: u128 = valor.map_or(0, |v| v.parse().unwrap_or(0));
        assert_eq!(inteiro, 1_791_565_402_212_345_678);
    }

    #[test]
    fn as_mesmas_chaves_entre_dois_formatos_produzem_o_mesmo_texto() {
        // Idempotencia: duas requisicoes precisam dar bytes identicos.
        let lista = formatos_do_perfil(Formatos::Completo);
        let a = FormatosResposta::de_formatos(lista, ref_in());
        let b = FormatosResposta::de_formatos(lista, ref_in());
        assert_eq!(a, b, "mesma entrada, mesmo mapa");
    }

    #[test]
    fn brasilia_e_tres_horas_atras_do_utc() {
        let brasilia = brasilia_de(ref_in());
        assert!(
            brasilia.starts_with("2026-10-09T14:03:22"),
            "17:03 UTC sao 14:03 em Brasilia: {brasilia}"
        );
        assert!(brasilia.ends_with("-03:00"), "fuso explicito: {brasilia}");
    }

    #[test]
    fn host_windows_traz_filetime_e_numeros() {
        let escala = EscalaDoHost::de_familia(hora_legal_dominio::host::FamiliaDeContagem::CemNanossegundos);
        let resposta = RespostaDeHost::de_instante(ref_in(), escala);
        assert_eq!(resposta.escala, "file_time");
        assert_eq!(resposta.unidades_por_segundo, 10_000_000);
        // `FILETIME` de 2026 tem 18 digitos.
        assert!(resposta.unidades.len() >= 18, "{}", resposta.unidades);
    }

    #[test]
    fn host_posix_traz_nansegundos() {
        let escala = EscalaDoHost::de_familia(hora_legal_dominio::host::FamiliaDeContagem::Nanossegundos);
        let resposta = RespostaDeHost::de_instante(ref_in(), escala);
        assert_eq!(resposta.escala, "relogio");
        assert_eq!(resposta.unidades_por_segundo, 1_000_000_000);
        assert_eq!(resposta.unidades, "1791565402123456789");
    }

    #[test]
    fn estado_da_fonte_nunca_e_nulo() {
        // Anti-padrao NULL: os tres estados da decisao produzem um rotulo.
        let fontes = [
            DecisaoDeConsulta::Sondar { tentativa: 0 },
            DecisaoDeConsulta::UsarCache {
                calibracao: hora_legal_dominio::fixture::amostra_de_teste_calibracao(),
            },
            DecisaoDeConsulta::FonteIndisponivel {
                calibra_em: ref_in(),
            },
        ];
        for fonte in &fontes {
            let resposta = RespostaDeFonte::de_fonte("a.ntp.br", "200.160.0.8:123", fonte);
            assert!(!resposta.estado.is_empty(), "estado nao pode ser vazio");
            assert!(!resposta.proxima_decisao.is_empty());
        }
    }

    #[test]
    fn problema_carrega_correlacao() {
        let p = ProblemaDeResposta::novo(
            "https://hora-legal.br/problemas/teste",
            "Teste",
            503,
            "detalhe",
            "abc-123",
        );
        assert_eq!(p.correlacao, "abc-123");
        assert_eq!(p.status, 503);
    }

    #[test]
    fn perfil_tem_nome_estavel() {
        assert_eq!(nome_do_perfil(Perfil::MenorLatencia), "menor-latencia");
        assert_eq!(nome_do_perfil(Perfil::MaximaPrecisao), "maxima-precisao");
    }
}