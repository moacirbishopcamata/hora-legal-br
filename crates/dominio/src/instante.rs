//! O instante: o objeto mais basico do dominio.
//!
//! Decisao de representacao: [`InstanteUtc`] guarda `i64` de segundos desde
//! 1970 mais `u32` de nanosssegundos -- nao `f64`. `f64` tem 53 bits de
//! mantissa; o epoch NTP tem 32 bits de fracao, ou seja, resolveria no maximo
//! 19 digitos, e a partir de 2^53 s (285 anos) deixaria de distinguir
//! segundos. `i128` intermediario na conversao, `i64/u32` no armazenamento:
//! representacao *fechada*, sem `Option` e sem `f64`.

use std::fmt;
use std::time::Duration;

use crate::civil::DataHoraCivil;
use crate::contrato::ErroDeContrato;

/// Erro de escrita em um [`fmt::Write`].
///
/// ## Por que este tipo existe
///
/// `std::fmt::Error` e' um zero que significa "falhou" e nao diz **onde**.
/// Numa funcao de renderizacao de timestamp, saber que a escrita de RFC 1123
/// falhou nao basta: e' preciso saber que o destino estava cheio. Este enum e'
/// a traducao, e existe para que o `?` funcione sem `map_err` repetido -- ver
/// [`Escrita`].
///
/// ## Por que `InstanteForaDoEpoch` vive aqui
///
/// Um chamador que renderiza formatos responde a **uma** pergunta: "devo
/// tentar outro formato?". Se `escrever` devolvesse
/// `ErroDeEscrita | ErroDeContrato`, todo `match` teria dois ramos quase
/// identicos e o `?` deixaria de funcionar. Unificando, um `?` resolve os
/// dois; quem quiser distinguir casa `InstanteForaDoEpoch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroDeEscrita {
    /// O destino recusou bytes (buffer cheio, writer quebrado).
    DestinoRecusou,
    /// O instante nao e' representavel no epoch do formato pedido.
    InstanteForaDoEpoch(ErroDeContrato),
}

impl fmt::Display for ErroDeEscrita {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinoRecusou => f.write_str("destino recusou bytes"),
            Self::InstanteForaDoEpoch(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ErroDeEscrita {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DestinoRecusou => None,
            Self::InstanteForaDoEpoch(e) => Some(e),
        }
    }
}

impl From<fmt::Error> for ErroDeEscrita {
    fn from(_: fmt::Error) -> Self {
        Self::DestinoRecusou
    }
}

impl From<ErroDeContrato> for ErroDeEscrita {
    fn from(e: ErroDeContrato) -> Self {
        Self::InstanteForaDoEpoch(e)
    }
}

/// Extensao que escreve em qualquer [`fmt::Write`] ja traduzindo o erro.
///
/// ## Por que uma trait e nao `?` direto
///
/// `fmt::Write::write_str` devolve `Result<(), fmt::Error>`. Com `?` num
/// arquivo cujo erro de retorno e' [`ErroDeEscrita`], cada chamada precisaria
/// de `.map_err(From::from)` -- repetido umas vinte vezes num arquivo de
/// renderizacao, e cada repeticao e' ruido que esconde o formato.
///
/// Com esta trait, `destino.txt("...")?` ja resolve a traducao. O codigo de
/// producao fica legivel, e o que salta aos olhos passa a ser a lista de
/// formatos.
pub trait Escrita {
    /// Escreve um texto, traduzindo a falha de escrita.
    fn txt(&mut self, texto: &str) -> Result<(), ErroDeEscrita>;

    /// Escreve um caractere, traduzindo a falha de escrita.
    fn caractere(&mut self, c: char) -> Result<(), ErroDeEscrita>;

    /// Escreve via `write_fmt`, traduzindo a falha de escrita.
    fn escreve(&mut self, campos: fmt::Arguments<'_>) -> Result<(), ErroDeEscrita>;
}

impl<W: fmt::Write + ?Sized> Escrita for W {
    fn txt(&mut self, texto: &str) -> Result<(), ErroDeEscrita> {
        self.write_str(texto).map_err(ErroDeEscrita::from)
    }

    fn caractere(&mut self, c: char) -> Result<(), ErroDeEscrita> {
        self.write_char(c).map_err(ErroDeEscrita::from)
    }

    fn escreve(&mut self, campos: fmt::Arguments<'_>) -> Result<(), ErroDeEscrita> {
        self.write_fmt(campos).map_err(ErroDeEscrita::from)
    }
}

/// Segundos entre 1900-01-01 (epoch NTP) e 1970-01-01 (epoch Unix).
pub const EPOCH_NTP_EM_SEGUNDOS_UNIX: i64 = 2_208_988_800;

/// Bits de fracao do timestamp NTP de 64 bits.
pub const BITS_DE_FRACAO_NTP: u32 = 32;

/// Ultima era NTP enderecavel: era 3 comeca em 2444-02-07.
///
/// Alem disso o timestamp de 64 bits deixa de ter significado, e decodificar
/// como se fosse uma hora plausivel seria aceitar lixo.
pub const ERA_NTP_MAXIMA: u64 = 3;

/// Timestamp NTP de 64 bits em ponto fixo (32 bits de segundo + 32 de fracao).
///
/// Existe separada de [`InstanteUtc`] por um motivo de costura: e' o tipo que
/// *atravessa* o fio UDP. Ele carrega a unidade, entao o codec nao precisa
/// repetir "2^-32 s" em comentario, e a aritmetica de fracao fica num lugar
/// so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SegundosNtp(u64);

impl SegundosNtp {
    /// Interpreta os 64 bits como era 0 (1900-01-01 .. 2036-02-07).
    ///
    /// Pre-condicao: **nenhuma** -- todo `u64` produz um instante. A escolha de
    /// era 0 segue a RFC 5905 secao 6 (SNTP); a deteccao de era correta fica
    /// na politica, que compara com o relogio local, e nao aqui. Uma funcao
    /// de decodificacao *nao pode* falhar, senao pacote malformado derruba o
    /// processo -- que e' exatamente o que acontece com `u32::from_be_bytes`
    /// followed de `as` sem conferencia.
    #[must_use]
    pub const fn de_bruto(bruto: u64) -> Self {
        Self(bruto)
    }

    /// Os 64 bits crus, prontos para o fio.
    #[must_use]
    pub const fn bruto(self) -> u64 {
        self.0
    }

    /// Era NTP (0..=3) a que este valor pertence, se decodificado como era 0.
    #[must_use]
    pub const fn era(self) -> u64 {
        self.0 >> BITS_DE_FRACAO_NTP
    }

    /// Segundos inteiros da era 0.
    #[must_use]
    pub const fn segundos_inteiros(self) -> u64 {
        self.0 >> BITS_DE_FRACAO_NTP
    }

    /// Fracao do segundo, 0 .. 2^32-1 (unidades de 2^-32 s).
    #[must_use]
    pub const fn fracao(self) -> u64 {
        self.0 & ((1 << BITS_DE_FRACAO_NTP) - 1)
    }

    /// Converte para instante UTC.
    ///
    /// Pos-condicao: o instante resultante tem a mesma posicao na linha do
    /// tempo, com precisao de ~233 ps (1 uL de 2^-32 s arredondado a ns).
    #[must_use]
    pub const fn em_instante(self) -> InstanteUtc {
        // Os 32 bits altos sao **segundos desde 1900**. Para obter segundos
        // desde 1970, subtrai-se o offset de 1900 -- porque 1900 e' anterior
        // a 1970, o valor UNIX correspondente e' negativo. Somar, como fiz
        // antes, devolvia 1900 + 68 anos.
        let segundos = (self.segundos_inteiros() as i64) - EPOCH_NTP_EM_SEGUNDOS_UNIX;
        let nanos = ((self.fracao() as u128 * 1_000_000_000) >> BITS_DE_FRACAO_NTP) as u32;
        InstanteUtc { segundos, nanos }
    }
}

/// Instante na escala UTC, com resolucao de nanosssegundo.
///
/// Invariante: `nanos < 1_000_000_000`. Os campos sao privados justamente
/// para tornar essa invariante impossivel de furar de fora -- e' o unico tipo
/// do dominio que se autoprotege, porque e' o unico que other camadas
/// construzem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct InstanteUtc {
    segundos: i64,
    nanos: u32,
}

impl InstanteUtc {
    /// Epoch Unix.
    #[must_use]
    pub const fn epoch() -> Self {
        Self {
            segundos: 0,
            nanos: 0,
        }
    }

    /// Constroi a partir de segundos e nanosssegundos.
    ///
    /// Pre-condicao: `nanos < 1_000_000_000`. Normalizar dentro do construtor
    /// seria tentador e esta errado -- um `nanos = 1_500_000_000` silenciosamente
    /// normalizado esconde o bug de quem montou o valor.
    pub fn de_segundos_e_nanos(segundos: i64, nanos: u32) -> Result<Self, ErroDeContrato> {
        if nanos >= 1_000_000_000 {
            return Err(ErroDeContrato::novo(
                "InstanteUtc::de_segundos_e_nanos",
                format!("nanos={nanos} fora de 0..1_000_000_000; nao normalizado"),
            ));
        }
        Ok(Self { segundos, nanos })
    }

    /// Instante a partir de milissegundos desde o epoch Unix (epoch HTTP).
    #[must_use]
    pub const fn de_milissegundos(millis: i64) -> Self {
        Self {
            segundos: millis.div_euclid(1_000),
            nanos: (millis.rem_euclid(1_000) * 1_000_000) as u32,
        }
    }

    /// Instante a partir do timestamp NTP de 64 bits (era 0). Total.
    #[must_use]
    pub const fn de_segundos_ntp(bruto: u64) -> Self {
        SegundosNtp::de_bruto(bruto).em_instante()
    }

    /// Instante a partir de `clock_gettime(CLOCK_REALTIME)`/`GetSystemTime...`:
    /// segundos e nanosssegundos ja separados.
    ///
    /// Pre-condicao: normalizacao e' obrigatoria porque a API do host promete
    /// `0 <= tv_nsec < 1e9`, mas um `Option` mal tratado do lado de fora pode
    /// nao cumprir. Aqui a entrada e' normalizada em vez de recusada -- ver
    /// [`InstanteUtc::de_segundos_e_nanos`] para o contraste.
    #[must_use]
    pub const fn de_relogio_do_host(segundos: i64, nanos: u64) -> Self {
        Self {
            segundos: segundos + (nanos / 1_000_000_000) as i64,
            nanos: (nanos % 1_000_000_000) as u32,
        }
    }

    /// Segundos desde o epoch Unix (parte inteira; o resto esta em `nanos`).
    #[must_use]
    pub const fn segundos_desde_epoch(self) -> i64 {
        self.segundos
    }

    /// Nanosssegundos dentro do segundo, `0..=999_999_999`.
    #[must_use]
    pub const fn nanos_do_segundo(self) -> u32 {
        self.nanos
    }

    /// Valor total em nanosssegundos desde o epoch Unix.
    ///
    /// Pre-condicao: o instante precisa caber em `i128`. Satisfeita para
    /// qualquer valor de `i64` de segundos (mais de 100 bilhoes de anos).
    pub fn nanos_totais(self) -> Result<i128, ErroDeContrato> {
        let total = i128::from(self.segundos)
            .checked_mul(1_000_000_000)
            .and_then(|s| s.checked_add(i128::from(self.nanos)));
        if total.is_none() {
            return Err(ErroDeContrato::novo(
                "InstanteUtc::nanos_totais",
                format!("segundos={} excede i128 em nanos", self.segundos),
            ));
        }
        Ok(total.unwrap_or(0))
    }

    /// Milissegundos desde o epoch Unix -- o `Date` numerico do HTTP e o
    /// formato que `Date`/`date -d @<ms>` comem.
    #[must_use]
    pub const fn milissegundos_desde_epoch(self) -> i64 {
        self.segundos
            .saturating_mul(1_000)
            .saturating_add((self.nanos / 1_000_000) as i64)
    }

    /// Timestamp NTP de 64 bits (era 0).
    ///
    /// Pre-condicao: o instante deve ser posterior a 1900-01-01. Antes disso
    /// a era 0 nao codifica o valor, e devolver `0` seria mentir.
    pub fn para_segundos_ntp(self) -> Result<u64, ErroDeContrato> {
        // Inverso de [`SegundosNtp::em_instante`]: 1900 e' anterior a 1970,
        // entao os segundos desde 1900 sao `unix + 2_208_988_800`. O epoch
        // Unix (0) corresponde a 2_208_988_800 segundos NTP -- e nao a zero.
        let deslocado = self.segundos + EPOCH_NTP_EM_SEGUNDOS_UNIX;
        if deslocado < 0 || (deslocado as u64) > ERA_NTP_MAXIMA << BITS_DE_FRACAO_NTP {
            return Err(ErroDeContrato::novo(
                "InstanteUtc::para_segundos_ntp",
                format!("instante {self:?} fora das eras NTP enderecaveis"),
            ));
        }
        let fracao = (u128::from(self.nanos) << BITS_DE_FRACAO_NTP) / 1_000_000_000;
        Ok(((deslocado as u64) << BITS_DE_FRACAO_NTP) | fracao as u64)
    }

    /// Instante deslocado **para frente** no tempo.
    ///
    /// **Pos-condicao:** `mais(d).delta_ate(self) == d.as_nanos()`.
    #[must_use]
    pub const fn mais(self, duracao: Duration) -> Self {
        Self::de_nanos_totais(self.nanos_totais_const() + duracao.as_nanos() as i128)
    }

    /// Instante deslocado **para tras** no tempo.
    ///
    /// **Pos-condicao:** `menos(d).delta_ate(self) == -d.as_nanos()`.
    #[must_use]
    pub const fn menos(self, duracao: Duration) -> Self {
        Self::de_nanos_totais(self.nanos_totais_const() - duracao.as_nanos() as i128)
    }

    /// Constroi a partir de nanossegundos **totais** desde o epoch Unix, com
    /// sinal.
    ///
    /// Total, e com normalizacao: `de_nanos_totais(-1)` e' 1969-12-31T23:59:59
    /// com 999999999 ns. A normalizacao usa `div_euclid`/`rem_euclid` porque
    /// `%` trunca para zero e, em negativo, produziria um instante com nanos
    /// negativos -- furando a invariante do tipo.
    #[must_use]
    pub const fn de_nanos_totais(total: i128) -> Self {
        Self {
            segundos: total.div_euclid(1_000_000_000) as i64,
            nanos: total.rem_euclid(1_000_000_000) as u32,
        }
    }

    /// Nanossegundos totais desde o epoch, em aritmetica de `const`.
    ///
    /// Versao *interna* e infallivel de [`Self::nanos_totais`]. As duas
    /// existem porque `i128` com segundos de `i64` nao transborda
    /// (2^94 nanos), mas `nanos_totais` devolve `Result` para responder ao
    /// contrato publico -- e um `Result` nao pode ser usado em `const fn`.
    const fn nanos_totais_const(self) -> i128 {
        (self.segundos as i128)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(self.nanos as i128)
    }

    /// Diferenca entre dois instantes, em nanossegundos, com sinal.
    ///
    /// **Pos-condicao:** `delta_ate(t) == -delta_ate(t).delta_ate(s)`. E' a
    /// operacao que a matematica do NTP usa, e por isso e' `i128`: o
    /// resultado intermediario de `(T2 - T1) + (T3 - T4)` passa de `i64`
    /// quando o intervalo medido e' da ordem de anos.
    ///
    /// Totality: nao ha pre-condicao -- a diferenca de dois instantes
    /// quaisquer sempre existe. Por isso `menos` (que exige `self >= outro`)
    /// e' que falha, e nao esta.
    #[must_use]
    pub const fn delta_ate(self, outro: Self) -> i128 {
        (outro.segundos as i128 - self.segundos as i128) * 1_000_000_000
            + (outro.nanos as i128 - self.nanos as i128)
    }

    /// Quanto tempo passou entre `self` e `agora`.
    ///
    /// Pre-condicao: `self <= agora`. Um "idade" negativo e' sempre bug de
    /// calculo -- e devolve erro, nao um `Duration` absurdo.
    pub fn idade(self, agora: Self) -> Result<Duration, ErroDeContrato> {
        let total = self.delta_ate(agora);
        // Os dois casos sao estados distintos do dominio -- idade valida e
        // idade negativa -- e o `Err` carrega o diagnostico que torna a falha
        // acionavel em vez de apenas visivel.
        match total < 0 {
            true => Err(ErroDeContrato::novo(
                "InstanteUtc::idade",
                format!("idade negativa: {total}ns (medido {self:?}, agora {agora:?})"),
            )),
            false => Ok(Duration::new(
                (total / 1_000_000_000) as u64,
                (total.rem_euclid(1_000_000_000)) as u32,
            )),
        }
    }

    /// Calendario civil em UTC (`deslocamento` zero, renderizado como `Z`).
    #[must_use]
    pub fn civil_utc(self) -> DataHoraCivil {
        self.civil_com_deslocamento(0)
    }

    /// Calendario civil num fuso de deslocamento fixo.
    ///
    /// Pre-condicao: `deslocamento` em [`DataHoraCivil::DESLOCAMENTO_MINIMO`] ..
    /// [`DataHoraCivil::DESLOCAMENTO_MAXIMO`].
    pub fn civil_com_deslocamento(self, deslocamento: i32) -> DataHoraCivil {
        let local = Self::de_relogio_do_host(
            self.segundos.saturating_add(i64::from(deslocamento)),
            u64::from(self.nanos),
        );
        let dias = local.segundos.div_euclid(86_400);
        let dentro_do_dia = local.segundos.rem_euclid(86_400);
        let (ano, mes, dia) = DataHoraCivil::de_dias_desde_epoch(dias);
        DataHoraCivil {
            ano,
            mes,
            dia,
            hora: (dentro_do_dia / 3_600) as u8,
            minuto: ((dentro_do_dia % 3_600) / 60) as u8,
            segundo: (dentro_do_dia % 60) as u8,
            nanos: local.nanos,
            deslocamento_segundos: deslocamento,
        }
    }
}

impl fmt::Display for InstanteUtc {
    /// Renderiza RFC 3339 em UTC. Aloca -- use [`InstanteUtc::civil_utc`] com
    /// um buffer reaproveitado em caminho quente.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Ver `DataHoraCivil::fmt`: o trait `Display` exige `fmt::Error`, e a
        // granularidade se perde aqui -- por decisao do proprio Rust.
        self.civil_utc()
            .escrever_rfc3339(f)
            .map_err(|_| fmt::Error)
    }
}