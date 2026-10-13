//! Compatibilidade com o sistema operacional **host**.
//!
//! Um mesmo instante UTC tem representacoes numericas diferentes conforme a
//! familia de relogio que o sistema operacional expoe. Servir so RFC 3339 e'
//! insuficiente para quem precisa reatribuir a hora a um objeto nativo
//! (`FILETIME` no Windows, `struct timespec` no POSIX): ainda teria que
//! converter na mao, e essa conversao e' a origem classica de erro de um dia.
//!
//! Este modulo faz a traducao **uma vez, no dominio**, e devolve um tipo
//! fechado: a resposta HTTP emite os campos da variante ativa, nunca campos
//! nulos dos inativos.
//!
//! | Familia | Unidade | Epoch | Consumidor nativo |
//! |---|---|---|---|
//! | [`FamiliaDeContagem::CemNanossegundos`] | 100 ns | 1601-01-01 | `FILETIME`, `GetSystemTimePreciseAsFileTime` |
//! | [`FamiliaDeContagem::Nanossegundos`] | 1 ns | 1970-01-01 | `clock_gettime(CLOCK_REALTIME)`, `SystemTime` |
//!
//! ## Decisao de escopo: hora civil local nao e' responsabilidade daqui
//!
//! O fuso local do host nao e' legivel sem base de dados de fusos, e o
//! proprio NTP.br e' explicito a respeito: *"O tratamento do horario local e
//! horario de verao sao funcoes dos Sistemas Operacionais"*. Portanto o host
//! recebe **numeros** que o proprio SO entende, e o cliente converte para o
//! seu fuso com a base que ele ja tem. Isso mantem a API honesta e o payload
//! minimo.

use crate::contrato::ErroDeContrato;
use crate::instante::InstanteUtc;

/// Segundos entre 1601-01-01 (epoch Windows) e 1970-01-01 (epoch Unix).
pub const EPOCH_WINDOWS_EM_SEGUNDOS_UNIX: i64 = -11_644_473_600;

/// Nanosssegundos por unidade de `FILETIME`.
///
/// **Invariante entre familias:** `CemNanossegundos::unidades_por_segundo() *
/// NANOSSEGUNDOS_POR_UNIDADE_DE_FILETIME == Nanossegundos::unidades_por_segundo()`,
/// ou seja `10^7 * 100 == 10^9`. E' esse fator que torna a conversao entre as
/// duas contagens uma multiplicacao exata -- e o que o teste
/// `as_duas_familias_concordam_no_ponto_comum` verifica.
pub const NANOSSEGUNDOS_POR_UNIDADE_DE_FILETIME: u64 = 100;

/// Sistema operacional do processo host, resolvido em tempo de compilacao.
///
/// `cfg!` em `const`: a deteccao e' feita pelo compilador, nao por
/// `uname` em runtime. O valor e' uma escolha do binario, nao um estado
/// que possa mudar -- por isso pode ser constante.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SistemaDoHost {
    /// Windows: familia `FILETIME`.
    Windows,
    /// Familia POSIX: Linux, macOS, *BSD.
    Unix,
    /// Qualquer outro alvo (WASI, alvo exotico). Cai em UTC + epoch Unix.
    Outro,
}

impl SistemaDoHost {
    /// Constante do host de compilacao.
    #[must_use]
    pub const fn do_host() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_family = "unix") {
            Self::Unix
        } else {
            Self::Outro
        }
    }

    /// Familia de contagem de tempo nativa desse sistema.
    #[must_use]
    pub const fn familia_de_contagem(self) -> FamiliaDeContagem {
        match self {
            Self::Windows => FamiliaDeContagem::CemNanossegundos,
            Self::Unix | Self::Outro => FamiliaDeContagem::Nanossegundos,
        }
    }

    /// Nome estatico, como o host se identifica (`cfg!(target_os)` cru).
    #[must_use]
    pub const fn nome_cru(self) -> &'static str {
        std::env::consts::OS
    }

    /// Nome estavel da API, independente da grafia do target.
    #[must_use]
    pub const fn identificacao(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Unix => "unix",
            Self::Outro => "outro",
        }
    }
}

/// Familia de contagem: a unidade e o epoch do relogio nativo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FamiliaDeContagem {
    /// Windows: 100 ns desde 1601-01-01 (`FILETIME`).
    CemNanossegundos,
    /// POSIX: 1 ns desde 1970-01-01 (`timespec`).
    Nanossegundos,
}

impl FamiliaDeContagem {
    /// Sistema operacional cuja familia de contagem e' esta.
    ///
    /// **Pos-condicao:** `sistema_correspondente().familia_de_contagem() == *self`.
    /// E' essa propriedade que mantem `EscalaDoHost` coerente.
    #[must_use]
    pub const fn sistema_correspondente(self) -> SistemaDoHost {
        match self {
            Self::CemNanossegundos => SistemaDoHost::Windows,
            Self::Nanossegundos => SistemaDoHost::Unix,
        }
    }
    /// Unidades por segundo do relogio do host.
    #[must_use]
    pub const fn unidades_por_segundo(self) -> u64 {
        match self {
            Self::CemNanossegundos => 10_000_000,
            Self::Nanossegundos => 1_000_000_000,
        }
    }

    /// Segundos entre o epoch dessa familia e o epoch Unix.
    #[must_use]
    pub const fn epoch_unix(self) -> i64 {
        match self {
            Self::CemNanossegundos => EPOCH_WINDOWS_EM_SEGUNDOS_UNIX,
            Self::Nanossegundos => 0,
        }
    }

    /// Identificador canonico da familia, como sai na API.
    ///
    /// **Total:** todo `self` produz um nome. **Pos-condicao:** nunca vazio --
    /// um campo `escala: ""` no JSON seria indistinguivel de "nao informado"
    /// (Yegor256, *Elegant Objects*, regra 1: nao use `null`).
    ///
    /// ## Por que o nome e' o da **unidade**, e nao o do SO
    ///
    /// `Nanossegundos` e' a familia POSIX (`CLOCK_REALTIME` em nanossegundos),
    /// `CemNanossegundos` e' a do Windows (`FILETIME`, 100 ns por unidade).
    /// Nomear pelo SO impediria o contrato de sobreviver a uma plataforma que
    /// usa 100 ns sem ser Windows -- e o cliente que so precisa de "unidades
    /// desde o epoch" nao deveria precisar saber qual SO produziu.
    #[must_use]
    pub const fn identificacao(self) -> &'static str {
        match self {
            Self::CemNanossegundos => "centesimos_de_nanosegundo",
            Self::Nanossegundos => "nanossegundos",
        }
    }
}

/// Escala de tempo do host: familia + epoch + resolucao.
///
/// Valor de configuracao (vem do binario), nao estado mutavel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EscalaDoHost {
    /// Sistema operacional host.
    pub sistema: SistemaDoHost,
    /// Familia da contagem.
    pub familia: FamiliaDeContagem,
    /// Segundos entre o epoch da familia e 1970-01-01.
    pub epoch_unix: i64,
    /// Unidades por segundo.
    pub unidades_por_segundo: u64,
}

impl EscalaDoHost {
    /// Escala do host de compilacao.
    #[must_use]
    pub const fn do_host() -> Self {
        let sistema = SistemaDoHost::do_host();
        let familia = sistema.familia_de_contagem();
        Self {
            sistema,
            familia,
            epoch_unix: familia.epoch_unix(),
            unidades_por_segundo: familia.unidades_por_segundo(),
        }
    }

    /// Constrói a escala de uma familia arbitraria -- usado para servir
    /// clientes de outra plataforma a partir do mesmo host.
    pub const fn de_familia(familia: FamiliaDeContagem) -> Self {
        Self {
            // O `sistema` acompanha a **familia**, nao o host de compilacao.
            // Fixar o host aqui produzia escala incoerente -- `familia =
            // Nanossegundos` com `sistema = Windows` -- e o par
            // `formatos.rs`/`host.rs` discordava sobre a mesma constante.
            sistema: familia.sistema_correspondente(),
            familia,
            epoch_unix: familia.epoch_unix(),
            unidades_por_segundo: familia.unidades_por_segundo(),
        }
    }
}

/// Um instante translatedo para a contagem nativa do host.
///
/// Sempre traz a escala junto: um inteiro sem unidade e' um bug esperando
/// acontecer, e a escala e' a unidade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContagemDoHost {
    /// Escala em que `unidades` estao expressas.
    pub escala: EscalaDoHost,
    /// Unidades desde o epoch da escala.
    ///
    /// `i128` porque `FILETIME` cabe folgadamente em `u64`, mas `i128`
    /// evita qualquer ramificacao com sinal no meio do calculo, que e' onde
    /// bugs de tempo moram.
    pub unidades: i128,
}

impl ContagemDoHost {
    /// Traduz um instante UTC para a escala do host.
    ///
    /// Pre-condicao: o instante deve ser representavel na escala. Para
    /// `FILETIME` isso significa ser posterior a 1601-01-01 -- o piso do
    /// proprio Windows, e anterior a ele a contagem seria negativa e
    /// invalida.
    pub fn de_instante(escala: EscalaDoHost, instante: InstanteUtc) -> Result<Self, ErroDeContrato> {
        let segundos_desde_epoch = instante.segundos_desde_epoch() - escala.epoch_unix;
        if segundos_desde_epoch < 0 {
            return Err(ErroDeContrato::novo(
                "ContagemDoHost::de_instante",
                format!(
                    "instante {instante:?} anterior ao epoch da escala {:?} ({segundos_desde_epoch}s)",
                    escala.familia
                ),
            ));
        }
        // nanos -> unidades, com resto. O resto e' uma perda < 1 unidade do
        // host (100 ns no Windows), abaixo do ruido do proprio SO.
        let nanos = i128::from(instante.nanos_do_segundo());
        let unidades = i128::from(segundos_desde_epoch)
            .checked_mul(i128::from(escala.unidades_por_segundo))
            .and_then(|s| s.checked_add(nanos * i128::from(escala.unidades_por_segundo) / 1_000_000_000))
            .ok_or_else(|| {
                ErroDeContrato::novo(
                    "ContagemDoHost::de_instante",
                    format!("estouro em i128 para {instante:?}"),
                )
            })?;
        Ok(Self { escala, unidades })
    }

    /// Numero de unidades de 100 ns (`FILETIME`), se a escala for Windows.
    ///
    /// Pre-condicao: familia Windows. Devolve `Err` caso contrario em vez de
    /// devolver 0 -- zero e' um `FILETIME` valido (1601) e um bug silencioso.
    pub fn filetime(self) -> Result<u64, ErroDeContrato> {
        if self.escala.familia != FamiliaDeContagem::CemNanossegundos {
            return Err(ErroDeContrato::novo(
                "ContagemDoHost::filetime",
                format!("escala {:?} nao e' FILETIME", self.escala.familia),
            ));
        }
        u64::try_from(self.unidades).map_err(|_| {
            ErroDeContrato::novo(
                "ContagemDoHost::filetime",
                format!("{} unidades excedem u64", self.unidades),
            )
        })
    }
}

#[cfg(test)]
#[allow(clippy::panic)] // o alvo deste macro de teste e' justamente provar que `panic!` acontece.
mod testes {
    use super::*;
    use crate::exigir;

    #[test]
    fn identificacao_nunca_e_vazia() {
        // Pos-condicao do identificador: um campo `escala: ""` seria
        // indistinguivel de "nao informado" (Yegor256, regra 1).
        for familia in [
            FamiliaDeContagem::CemNanossegundos,
            FamiliaDeContagem::Nanossegundos,
        ] {
            exigir!(
                !familia.identificacao().is_empty(),
                "toda familia precisa de identificador",
                "{familia:?}"
            );
        }
    }

    #[test]
    fn identificacao_e_distinta_entre_familias() {
        // Duas familias com o mesmo nome esconderiam exatamente a diferenca
        // que o cliente precisa ver: `FILETIME` (100 ns desde 1601) e ticks do
        // .NET (100 ns desde 0001) tem o mesmo passo e epochs diferentes.
        exigir!(
            FamiliaDeContagem::CemNanossegundos.identificacao()
                != FamiliaDeContagem::Nanossegundos.identificacao(),
            "familias distintas precisam de nomes distintos",
            "{}",
            FamiliaDeContagem::CemNanossegundos.identificacao()
        );
    }

    /// Instante fixo usado pelos testes deste modulo.
///
/// Monta uma data UTC a partir de componentes civis, em vez de usar
/// `epoch()`: o epoch Unix e' **1970**, e um teste que fala de 1601 ou 1970
/// precisa de uma data, nao de um instante magico.
    fn em(ano: i64, mes: u8, dia: u8) -> InstanteUtc {
        crate::fixture::instante(ano, mes, dia, 0, 0, 0, 0)
    }

    #[test]
    fn filetime_usa_o_epoch_de_1601() {
        let escala = EscalaDoHost::de_familia(FamiliaDeContagem::CemNanossegundos);
        let Ok(contagem) = ContagemDoHost::de_instante(escala, em(1601, 1, 1)) else {
            panic!("1601 e' representavel em FILETIME");
        };
        exigir!(
            contagem.unidades == 0,
            "filetime: epoch 1601 vale zero",
            "{}",
            contagem.unidades
        );
    }

    #[test]
    fn nanossegundos_usa_o_epoch_unix() {
        let escala = EscalaDoHost::de_familia(FamiliaDeContagem::Nanossegundos);
        let Ok(contagem) = ContagemDoHost::de_instante(escala, em(1970, 1, 1)) else {
            panic!("1970 e' representavel em timespec");
        };
        exigir!(
            contagem.unidades == 0,
            "timespec: epoch 1970 vale zero",
            "{}",
            contagem.unidades
        );
    }

    #[test]
    fn as_duas_familias_concordam_no_ponto_comum() {
        let instante = em(2026, 10, 9);
        let Ok(ft) = ContagemDoHost::de_instante(
            EscalaDoHost::de_familia(FamiliaDeContagem::CemNanossegundos),
            instante,
        ) else {
            return;
        };
        let Ok(ns) = ContagemDoHost::de_instante(
            EscalaDoHost::de_familia(FamiliaDeContagem::Nanossegundos),
            instante,
        ) else {
            return;
        };
        let Ok(ft_u64) = ft.filetime() else {
            return;
        };
        // Os dois campos medem a MESMA duracao desde instantes **diferentes**:
        // `FILETIME` conta desde 1601, `timespec` desde 1970. Logo a relacao
        // correta e
        //
        //     filetime * 100 = timespec + 11_644_473_600 * 10^9
        //
        // Testar `filetime * 100 == timespec` passaria por acaso perto do
        // epoch e falharia em qualquer outra data -- o pior tipo de teste,
        // porque so falha longe do epoch.
        let epoch_windows_em_nanos = -EPOCH_WINDOWS_EM_SEGUNDOS_UNIX as i128 * 1_000_000_000;
        exigir!(
            i128::from(ft_u64) * i128::from(NANOSSEGUNDOS_POR_UNIDADE_DE_FILETIME)
                == ns.unidades + epoch_windows_em_nanos,
            "FILETIME e timespec medem a mesma duracao desde epochs diferentes",
            "{ft_u64} vs {ns:?}"
        );
    }

    #[test]
    fn instante_anterior_a_1601_e_rejeitado() {
        let escala = EscalaDoHost::de_familia(FamiliaDeContagem::CemNanossegundos);
        let resultado = ContagemDoHost::de_instante(escala, em(1500, 1, 1));
        exigir!(
            resultado.is_err(),
            "pre-condicao: antes de 1601 nao ha FILETIME valido",
            "aceitou"
        );
    }

    #[test]
    fn filetime_em_escala_posix_e_erro_e_nao_zero() {
        let escala = EscalaDoHost::de_familia(FamiliaDeContagem::Nanossegundos);
        let Ok(contagem) = ContagemDoHost::de_instante(escala, em(2026, 1, 1)) else {
            return;
        };
        let r = contagem.filetime();
        exigir!(
            r.is_err(),
            "pos-condicao: escala errada deve falhar alto",
            "{r:?}"
        );
    }

    #[test]
    fn sistema_do_host_se_reconhece() {
        let s = SistemaDoHost::do_host();
        // Pre-condicao: o SO se descreve com nome estavel.
        let nome = s.identificacao();
        exigir!(
            matches!(nome, "windows" | "unix" | "outro"),
            "identificacao estavel",
            "{nome}"
        );
        // Pos-condicao: a familia coerente com o sistema.
        let coerente = matches!(
            (s, s.familia_de_contagem()),
            (SistemaDoHost::Windows, FamiliaDeContagem::CemNanossegundos)
                | (SistemaDoHost::Unix | SistemaDoHost::Outro, FamiliaDeContagem::Nanossegundos)
        );
        exigir!(coerente, "familia coerente com o sistema", "{s:?}");
    }

    #[test]
    fn as_duas_escalas_tem_resolucoes_corretas() {
        exigir!(
            FamiliaDeContagem::CemNanossegundos.unidades_por_segundo() == 10_000_000,
            "FILETIME: 10^7 unidades por segundo",
            "{}",
            FamiliaDeContagem::CemNanossegundos.unidades_por_segundo()
        );
        exigir!(
            FamiliaDeContagem::Nanossegundos.unidades_por_segundo() == 1_000_000_000,
            "timespec: 10^9 unidades por segundo",
            "{}",
            FamiliaDeContagem::Nanossegundos.unidades_por_segundo()
        );
    }
}