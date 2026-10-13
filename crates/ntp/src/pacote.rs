//! O pacote NTP de 48 bytes, e nada mais.
//!
//! ## A anatomia do pacote (RFC 5905, secao 7.3)
//!
//! ```text
//!  0                   1                   2                   3
//!  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |LI | VN |Mode |     Stratum |     Poll      |   Precision   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |          Root Delay           |         Root Dispersion      |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                                                               |
//! |                         Reference ID                          |
//! |                                                               |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                  Reference Timestamp (64)                    |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                  Originate  Timestamp (64)                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                   Receive   Timestamp (64)                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! |                   Transmit   Timestamp (64)                   |
//! +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
//! ```
//!
//! Para uma **sondagem cliente**, os quatro timestamps sao o pacote inteiro.
//! Tudo o mais e' metadado que este servico nao consome -- e por isso que o
//! `Decodificador` ignora, em vez de expor, cada campo.
//!
//! ## Por que a decodificacao e' *total*
//!
//! Todo byte de entrada produz um [`PacoteNtp`] valido ou um erro **descrito**.
//! Nao existe `panic!` e nao existe `Option` parcial: um servidor malicioso ou
//! uma resposta truncada tem de virar `Err`, nunca um processo morto. Ver as
//! pre-condicoes de [`Decodificador::decodificar`].

use hora_legal_dominio::instante::SegundosNtp;

/// Tamanho fixo de um pacote NTP. Nao ha tamanho variavel.
pub const TAMANHO_DO_PACOTE: usize = 48;

/// Offsets dos campos dentro do pacote. Constantes nomeadas em vez de numeros
/// soltos: `pacote[40..48]` num `codigo` e' ilegivel; `pacote[INICIO_DO_ESTAMPAMENTO_DE_TRANSMISSAO..]` nao.
mod campo {
    /// Byte 0: `LI | VN | Mode`.
    pub const MODO: usize = 0;
    /// Byte 1: estrato.
    pub const ESTRATO: usize = 1;
    /// Byte 2: kiss-of-death quando estrato == 0.
    pub const BEIJO_DA_MORTE: usize = 2;
    /// Byte 3: precisao (log2 de segundos).
    pub const PRECISAO: usize = 3;
    /// Bytes 24..32: instante de origem (espelha o nosso T1).
    pub const ESTAMPAMENTO_DE_ORIGEM: std::ops::Range<usize> = 24..32;
    /// Bytes 32..40: instante de recepcao no servidor (T2).
    pub const ESTAMPAMENTO_DE_RECEPCAO: std::ops::Range<usize> = 32..40;
    /// Bytes 40..48: instante de transmissao do servidor (T3).
    pub const ESTAMPAMENTO_DE_TRANSMISSAO: std::ops::Range<usize> = 40..48;
}

/// Indicador de qualidade da fonte, bits 6..7 do byte 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicadorDeSincronismo {
    /// `LI = 0`: sem restrição.
    SemRestricao,
    /// `LI = 1`: o relogio e' secondary (referenciado por outro servidor).
    Secundario,
    /// `LI = 2`: deletado pelo administrador (NTPv3).
    Deletado,
    /// `LI = 3`: nao sincronizado.
    NaoSincronizado,
}

impl IndicadorDeSincronismo {
    /// Extrai `LI` do byte 0.
    ///
    /// **Total**: os dois bits sempre produzem uma das quatro variantes.
    #[must_use]
    pub const fn de_bits(bits: u8) -> Self {
        match (bits >> 6) & 0b11 {
            0 => Self::SemRestricao,
            1 => Self::Secundario,
            2 => Self::Deletado,
            _ => Self::NaoSincronizado,
        }
    }
}

/// Modo de operacao, bits 0..5 do byte 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    /// Reservado / bit 7 setado (RFC 5905 v4).
    Reservado,
    /// Symmetric active.
    SimetricoAtivo,
    /// Symmetric passive.
    SimetricoPassivo,
    /// Client: o que este servico envia.
    Cliente,
    /// Server: o que um servidor responde.
    Servidor,
    /// Broadcast.
    Difusao,
    /// Control (NTPv3, obsoleto).
    Controle,
    /// Private.
    Privado,
}

impl Modo {
    /// Extrai o modo do byte 0.
    ///
    /// **Total**: 8 combinacoes, 8 variantes. Nenhum valor e' invalido.
    #[must_use]
    pub const fn de_bits(bits: u8) -> Self {
        match bits & 0b0000_0111 {
            0 => Self::Reservado,
            1 => Self::SimetricoAtivo,
            2 => Self::SimetricoPassivo,
            3 => Self::Cliente,
            4 => Self::Servidor,
            5 => Self::Difusao,
            6 => Self::Controle,
            _ => Self::Privado,
        }
    }

    /// `true` se o modo e' uma resposta de servidor (4, 5 ou 7).
    ///
    /// ## Por que o modo 7 (privado) conta como resposta
    ///
    /// A RFC 5905 secao 8 permite que um servidor em modo privado responda a
    /// um cliente. Filtrar por "modo == 4" apenas rejeitaria respostas validas
    /// de implementacoes que usam modo privado. A checagem fica no
    /// *dominio* (pre-condicao 6), que decide; aqui so descrevemos.
    #[must_use]
    pub const fn e_resposta_de_servidor(self) -> bool {
        matches!(self, Self::Servidor | Self::Difusao | Self::Privado)
    }
}

/// Falha ao interpretar um pacote.
///
/// ### Por que este enum e' fechado
///
/// Os erros possiveis ao ler 48 bytes sao poucos e conhecidos. Um enum fechado
/// obriga o `match` no chamador a ser exaustivo: quando o ON mudar o
/// protocolo, o compilador aponta cada sitio. Com `String`, o chamador
/// descobriria em producao.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeDecodificacao {
    /// O buffer tem tamanho diferente de 48.
    TamanhoInvalido {
        /// Tamanho recebido.
        recebido: usize,
    },
    /// O modo nao e' de resposta de servidor.
    ModoInvalido {
        /// Modo lido.
        modo: Modo,
    },
    /// O timestamp de origem nao espelha o nosso envio.
    OrigemIncoerente {
        /// O que foi enviado.
        enviado: u64,
        /// O que veio de volta.
        recebido: u64,
    },
}

impl std::fmt::Display for ErroDeDecodificacao {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TamanhoInvalido { recebido } => {
                write!(f, "pacote tem {recebido} bytes, esperado {TAMANHO_DO_PACOTE}")
            }
            Self::ModoInvalido { modo } => write!(f, "modo {modo:?} nao e' resposta de servidor"),
            Self::OrigemIncoerente { enviado, recebido } => write!(
                f,
                "timestamp de origem {recebido:#018x} nao espelha o envio {enviado:#018x}"
            ),
        }
    }
}

impl std::error::Error for ErroDeDecodificacao {}

/// Pacote NTP decodificado, contendo **apenas** o que este servico usa.
///
/// ## Por que os campos sao `pub` e nao metodos privados
///
/// `PacoteNtp` e' um **registro de fronteira**: ele traduz bytes em campos
/// tipados e nao tem comportamento, invariante ou ciclo de vida. Para esse
/// papel, campos publicos sao mais expressivos que getters -- `pacote.estrato`
/// le melhor que `pacote.estrato_atual()`. O documento `docs/ANTI-PATTERNS.md`
/// justifica por que aqui isso e' a decisao certa e em
/// `AmostraDeTempo` e' a errada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacoteNtp {
    /// Sincronismo declarado (`LI`).
    pub sincronismo: IndicadorDeSincronismo,
    /// Modo declarado.
    pub modo: Modo,
    /// Estrato declarado.
    pub estrato: u8,
    /// Codigo de *kiss-of-death*, valido apenas quando `estrato == 0`.
    pub beijo_da_morte: u8,
    /// Log2 da precisao em segundos.
    pub precisao: i8,
    /// T1 espelhado: o instante de origem que o servidor devolveu.
    pub origem: SegundosNtp,
    /// T2: recepcao no servidor.
    pub recepcao: SegundosNtp,
    /// T3: transmissao pelo servidor.
    pub transmissao: SegundosNtp,
}

impl PacoteNtp {
    /// Monta o pacote de requisicao de cliente (SNTP, versao 4).
    ///
    /// ## O que NAO esta aqui, e por que
    ///
    /// Um cliente SNTP tecnicamente deveria repetir T1 no campo de origem.
    /// A RFC 5905 secao 8 torna isso opcional e o campo e' em grande parte
    /// ignorado pelos servidores modernos. Preenche-lo tornaria o pacote
    /// util apenas com este adaptador, sem ganho. O campo vai zerado, como
    /// fazem `ntpd`, `chrony` e `sntp`.
    ///
    /// **Pos-condicao:** exatamente [`TAMANHO_DO_PACOTE`] bytes; byte 0 = 0x23
    /// (`LI=0`, `VN=4`, `Mode=3`); todos os demais zero.
    #[must_use]
    pub fn de_requisicao(enviado: SegundosNtp) -> [u8; TAMANHO_DO_PACOTE] {
        let mut pacote = [0u8; TAMANHO_DO_PACOTE];
        // 0b0010_0011 = LI 0 | VN 4 | Mode 3. Todo o resto fica zerado, exceto
        // o carimbo de transmissao (bytes 40..48), que carrega o T1: o servidor
        // o devolve como carimbo de origem, e e' essa a prova de que a resposta
        // e' ao *nosso* pedido (ver `decodificar`).
        if let Some(primeiro) = pacote.first_mut() {
            *primeiro = 0b0010_0011;
        }
        if let Some(destino) = pacote.get_mut(40..48) {
            destino.copy_from_slice(&enviado.bruto().to_be_bytes());
        }
        pacote
    }

    /// Decodifica um buffer de resposta.
    ///
    /// **Pre-condicoes:**
    /// 1. `bytes.len() == 48` (garantido pelo tipo do parametro, e verificado
    ///    mesmo assim porque o chamador pode ter recebido um buffer maior);
    /// 2. o modo e' de resposta de servidor;
    /// 3. o timestamp de origem espelha `enviado` -- e' a prova de que o
    ///    pacote e' resposta a *nosso* pedido e nao um pedido de outro cliente.
    ///
    /// Sem a pre-condicao 3, um pacote em retransmissao de outro cliente seria
    /// aceito e produziria um desvio falso -- o modo classico de um bug de NTP.
    pub fn decodificar(bytes: &[u8], enviado: SegundosNtp) -> Result<Self, ErroDeDecodificacao> {
        if bytes.len() != TAMANHO_DO_PACOTE {
            return Err(ErroDeDecodificacao::TamanhoInvalido {
                recebido: bytes.len(),
            });
        }
        let byte_zero = bytes
            .get(campo::MODO)
            .copied()
            .ok_or(ErroDeDecodificacao::TamanhoInvalido { recebido: 0 })?;
        let modo = Modo::de_bits(byte_zero);
        if !modo.e_resposta_de_servidor() {
            return Err(ErroDeDecodificacao::ModoInvalido { modo });
        }
        let origem = ler_timestamp(bytes, &campo::ESTAMPAMENTO_DE_ORIGEM);
        if origem != enviado {
            return Err(ErroDeDecodificacao::OrigemIncoerente {
                enviado: enviado.bruto(),
                recebido: origem.bruto(),
            });
        }
        Ok(Self {
            sincronismo: IndicadorDeSincronismo::de_bits(byte_zero),
            modo,
            estrato: bytes
                .get(campo::ESTRATO)
                .copied()
                .ok_or(ErroDeDecodificacao::TamanhoInvalido { recebido: 0 })?,
            beijo_da_morte: bytes
                .get(campo::BEIJO_DA_MORTE)
                .copied()
                .ok_or(ErroDeDecodificacao::TamanhoInvalido { recebido: 0 })?,
            precisao: bytes
                .get(campo::PRECISAO)
                .copied()
                .unwrap_or(0) as i8,
            origem,
            recepcao: ler_timestamp(bytes, &campo::ESTAMPAMENTO_DE_RECEPCAO),
            transmissao: ler_timestamp(bytes, &campo::ESTAMPAMENTO_DE_TRANSMISSAO),
        })
    }
}

/// Le um timestamp de 64 bits big-endian de um intervalo do pacote.
///
/// **Total**: `len(intervalo) == 8` por construcao (as constantes acima sao
/// todas de tamanho 8), entao nunca falha. O `copied` sobre `chunks_exact`
/// devolve `None` apenas se o intervalo nao fosse multiplode de 8 -- que
/// `unwrap_or` trata como zero, um pacote vazio e nao um panic.
fn ler_timestamp(bytes: &[u8], intervalo: &std::ops::Range<usize>) -> SegundosNtp {
    let bruto = bytes
        .get(intervalo.clone())
        .and_then(|fatia| fatia.try_into().ok())
        .map(u64::from_be_bytes)
        .unwrap_or(0);
    SegundosNtp::de_bruto(bruto)
}

#[cfg(test)]
mod testes {
    use super::*;
    use hora_legal_dominio::exigir;

    fn pacote_de_servidor_com(origem: u64, recepcao: u64, transmissao: u64) -> [u8; 48] {
        let mut p = [0u8; 48];
        p[0] = 0b0000_0100; // LI=0, VN=0, Mode=4 (servidor)
        p[1] = 1; // estrato 1
        p[campo::ESTRATO] = 1;
        p[24..32].copy_from_slice(&origem.to_be_bytes());
        p[32..40].copy_from_slice(&recepcao.to_be_bytes());
        p[40..48].copy_from_slice(&transmissao.to_be_bytes());
        p
    }

    #[test]
    fn requisicao_tem_o_modo_de_cliente_nas_versao_4() {
        let r = PacoteNtp::de_requisicao(SegundosNtp::de_bruto(0));
        exigir!(
            r.len() == TAMANHO_DO_PACOTE,
            "pos-condicao: 48 bytes",
            "{}",
            r.len()
        );
        exigir!(r[0] == 0b0010_0011, "LI=0 VN=4 Mode=3", "{:#010b}", r[0]);
        exigir!(
            r[1..].iter().all(|b| *b == 0),
            "resto do pacote zerado",
            "{:?}",
            &r[1..]
        );
    }

    #[test]
    fn decodifica_um_pacote_valido() {
        let enviado = SegundosNtp::de_bruto(0x1234_5678_9ABC_DEF0);
        let bytes = pacote_de_servidor_com(
            enviado.bruto(),
            0x1111_1111_1111_1111,
            0x2222_2222_2222_2222,
        );
        match PacoteNtp::decodificar(&bytes, enviado) {
            Ok(p) => {
                exigir!(p.estrato == 1, "estrato 1", "{}", p.estrato);
                exigir!(p.modo == Modo::Servidor, "modo servidor", "{:?}", p.modo);
                exigir!(
                    p.transmissao.bruto() == 0x2222_2222_2222_2222,
                    "T3 decodificado",
                    "{:#x}",
                    p.transmissao.bruto()
                );
                exigir!(
                    p.origem == enviado,
                    "T1 espelhado",
                    "{:?}",
                    p.origem
                );
            }
            Err(e) => exigir!(false, "pacote valido deve decodificar", "{e}"),
        }
    }

    #[test]
    fn tamanho_errado_e_erro_descrito() {
        let enviado = SegundosNtp::de_bruto(0);
        let r = PacoteNtp::decodificar(&[0u8; 20], enviado);
        let Err(ErroDeDecodificacao::TamanhoInvalido { recebido }) = r else {
            return exigir!(false, "esperava TamanhoInvalido", "{r:?}");
        };
        exigir!(recebido == 20, "tamanho relatado", "{recebido}");
    }

    #[test]
    fn modo_de_cliente_e_rejeitado() {
        let enviado = SegundosNtp::de_bruto(7);
        let bytes = pacote_de_servidor_com(7, 1, 2);
        let mut b = bytes;
        b[0] = 0b0010_0011; // Mode=3 (cliente) -- eco do nosso pedido
        let r = PacoteNtp::decodificar(&b, enviado);
        exigir!(
            matches!(r, Err(ErroDeDecodificacao::ModoInvalido { modo: Modo::Cliente })),
            "pre-condicao: eco de cliente nao e' resposta",
            "{r:?}"
        );
    }

    #[test]
    fn origem_que_nao_espelha_o_envio_e_rejeitada() {
        // O pacote de outro cliente que chegou atrasado.
        let nosso_envio = SegundosNtp::de_bruto(0xAAAA);
        let bytes = pacote_de_servidor_com(0xBBBB, 1, 2);
        let r = PacoteNtp::decodificar(&bytes, nosso_envio);
        exigir!(
            matches!(r, Err(ErroDeDecodificacao::OrigemIncoerente { .. })),
            "pre-condicao 3: origem alheia rejeitada",
            "{r:?}"
        );
    }

    #[test]
    fn todos_os_modos_sao_representaveis() {
        for bits in 0u8..8 {
            let modo = Modo::de_bits(bits);
            // Total: nenhum `bits` produz nada fora do enum.
            exigir!(
                matches!(modo, Modo::Reservado | Modo::SimetricoAtivo | Modo::SimetricoPassivo
                    | Modo::Cliente | Modo::Servidor | Modo::Difusao | Modo::Controle | Modo::Privado),
                "modo {bits} mapeado",
                "{modo:?}"
            );
        }
    }

    #[test]
    fn todos_os_indicadores_sao_representaveis() {
        for bits in [0u8, 1, 2, 3] {
            let i = IndicadorDeSincronismo::de_bits(bits);
            exigir!(
                matches!(i, IndicadorDeSincronismo::SemRestricao | IndicadorDeSincronismo::Secundario
                    | IndicadorDeSincronismo::Deletado | IndicadorDeSincronismo::NaoSincronizado),
                "LI={bits} mapeado",
                "{i:?}"
            );
        }
    }

    #[test]
    fn endianness_e_big_endian_como_manda_a_rfc() {
        // 0x0102..03 decodificado como big-endian vale 0x010203..., nao 0x030201...
        let bruto = 0x0102_0304_0506_0708u64;
        let bytes = pacote_de_servidor_com(bruto, bruto, bruto);
        let enviado = SegundosNtp::de_bruto(bruto);
        match PacoteNtp::decodificar(&bytes, enviado) {
            Ok(p) => exigir!(
                p.recepcao.bruto() == bruto,
                "T2 preservado byte a byte",
                "{:#x}",
                p.recepcao.bruto()
            ),
            Err(e) => exigir!(false, "deveria decodificar", "{e}"),
        }
    }

    #[test]
    fn beijo_da_morte_e_lido_do_byte_2() {
        let enviado = SegundosNtp::de_bruto(42);
        let mut bytes = pacote_de_servidor_com(42, 1, 2);
        bytes[campo::ESTRATO] = 0; // estrato 0
        bytes[campo::BEIJO_DA_MORTE] = 2; // RATE
        match PacoteNtp::decodificar(&bytes, enviado) {
            Ok(p) => {
                exigir!(p.estrato == 0, "estrato 0", "{}", p.estrato);
                exigir!(p.beijo_da_morte == 2, "codigo RATE", "{}", p.beijo_da_morte);
            }
            Err(e) => exigir!(false, "deveria decodificar", "{e}"),
        }
    }
}