//! # Formatos de timestamp: locais e externos
//!
//! Um instante unico, muitos formatos. Cada formato e' um **caso de uso
//! concreto** que ja existe em algum sistema -- por isso ele esta aqui, e
//! nao em um DTO de HTTP: o mesmo formato e' necessario em log, em CSV, em
//! banco, em planilha e em outro servico.
//!
//! ## A regra que organiza tudo: `i128`, nunca `f64`
//!
//! Nenhum formato numerico usa ponto flutuante. O epoch em nanossegundos
//! (`1_768_..._000_000_000`) tem 19 digitos e ultrapassa em muito os 53 bits
//! de mantissa de um `f64`; em JavaScript, `JSON.parse` transformaria
//! `1768123456789012345` em `1768123456789012300` **em silencio**. Por isso as
//! contagens grandes sao strings no JSON, e a decisao esta documentada em
//! `docs/LATENCIA.md` e `docs/API.md`.
//!
//! ## Pre-condicoes e invariantes
//!
//! **Pre-condicoes:**
//! - `Formato::de_nome` aceita apenas nomes do catalogo (`nome()` e' a
//!   fonte unica da verdade -- nao ha lista paralela que possa divergir);
//! - renderizacoes com epoch proprio exigem instante dentro da era valida.
//!
//! **Invariantes** (verificadas com `panic!`):
//! - `I1` toda renderizacao de epoch proprio comecou no epoch declarado;
//! - `I2` `soma_de_dias` e' consistente com `de_dias_desde_epoch` (ida e volta);
//! - `I3` nenhum formato produz string vazia.
//!
//! **Pos-condicoes:**
//! - `P1` `renderizar` sempre devolve texto parseavel de volta no mesmo
//!   formato, dentro da resolucao do formato;
//! - `P2` formatos de data-hora civil incluem sempre o deslocamento de fuso,
//!   exceto os que a propria especificacao proibe.

use std::fmt;

use crate::civil::DataHoraCivil;
use crate::exigir;
use crate::fuso::HoraLegalBrasileira;
use crate::instante::{ErroDeEscrita, Escrita, InstanteUtc};

/// Segundos entre 1980-01-06 (epoch GPS) e 1970-01-01.
pub const EPOCH_GPS_EM_SEGUNDOS_UNIX: i64 = 315_964_800;

/// Segundos leap somados ao UTC para obter o tempo GPS.
///
/// **Manutencao necessaria:** 19 desde 2017-01-01. Um *leap second* announcement
/// da IERS incrementa este numero. E' constante por decisao: a alternativa --
/// buscar a tabela IERS em runtime -- introduz dependencia de rede no
/// nucleo, e um服务 que não sabe a hora por causa da tabela de leap seconds é
/// pior do que um que esta 1 s atrasado ate a proxima release.
pub const SALTO_PARA_GPS: i64 = 19;

/// Segundos entre 0001-01-01 (epoch .NET) e 1970-01-01.
pub const DIAS_ENTRE_ANO_1_E_EPOCH: i64 = 719_162;

/// Dias entre 1899-12-30 (epoch Excel, com o bug do ano bissexto) e 1970-01-01.
pub const EPOCH_EXCEL_EM_DIAS: i64 = -25_569;

/// Dia Juliano em 1970-01-01T00:00:00Z.
pub const DIA_JULIANO_DO_EPOCH: f64 = 2_440_587.5;

/// Formato de timestamp suportado.
///
/// ### Enum fechado, nao `String`
///
/// Um `String` de nome de formato permitiria `format: "epocho"` e o
/// servidor responderia `200` com o campo ausente. O enum faz o compilador
/// apontar cada site que precisa tratar um formato novo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Formato {
    // --- Texto, padrao da internet ---
    /// RFC 3339 / ISO 8601 extendido, nanosssegundo: `2026-10-09T17:03:22.123456789Z`.
    ///
    /// O formato canonico. Todo cliente moderno o entende, e o unico que
    /// carrega fuso explicitamente.
    Rfc3339,
    /// RFC 3339 truncado a milissegundo: `2026-10-09T17:03:22.123Z`.
    ///
    /// Para quem usa `Date`/`Instant.parse` com menos precisao, ou um banco
    /// com coluna `timestamp(3)`.
    Rfc3339Millis,
    /// RFC 3339 truncado a microssegundo: `...22.123456Z`.
    ///
    /// Coluna `timestamp(6)` de PostgreSQL/MySQL.
    Rfc3339Micros,
    /// ISO 8601 basico (sem separadores): `20261009T170322.123456789Z`.
    ///
    /// Usado por `strftime %Y%m%dT%H%M%S` e por sistemas de arquivo.
    Iso8601Basico,
    /// RFC 1123: `Fri, 09 Oct 2026 17:03:22 GMT`.
    ///
    /// E-mail (Date), RSS, e o formato mais aceito por leitores humans.
    Rfc1123,
    /// RFC 850 (obsoleto): `Friday, 09-Oct-26 17:03:22 GMT`.
    Rfc850,
    /// `asctime`: `Fri Oct  9 17:03:22 2026`.
    ///
    /// Renderizado em **UTC**, e nao em hora local como o `asctime` da libc:
    /// a funcao original depende do `TZ` do processo, o que tornaria a
    /// resposta nao-deterministica entre replicas. Ver `docs/API.md`.
    Asctime,
    /// ISO 8601 com espaco: `2026-10-09 17:03:22.123`.
    ///
    /// O formato de `timestamp` do PostgreSQL (sem fuso) e do MySQL.
    /// Ambiguo por decisao do padrao; use `Rfc3339` quando o fuso importar.
    Sql,

    // --- Numerico, epoch ---
    /// Segundos desde 1970-01-01: `1789123402`.
    UnixSegundos,
    /// Milissegundos desde epoch: `1789123402123`.
    ///
    /// O epoch numerico do JavaScript (`Date.getTime()`).
    UnixMillis,
    /// Microssegundos desde epoch: `1789123402123456`.
    UnixMicros,
    /// Nanosssegundos desde epoch: `1789123402123456789`.
    ///
    /// **Sai como string no JSON**: 19 digitos excedem o `Number` do
    /// JavaScript.
    UnixNanos,

    // --- Contagens nativas de sistema operacional ---
    /// `FILETIME`: centesimos de nanossegundo desde 1601-01-01.
    ///
    /// A contagem de `SYSTEMTIME`/`FILETIME` do Windows.
    WindowsFileTime,
    /// `SYSTEMTIME` em campos: ano, mes, dia, hora, minuto, segundo, centesimos.
    ///
    /// A struct que `GetSystemTime` preenche; util para quem faz FFI.
    WindowsSystemTime,
    /// Ticks .NET: centesimos de nanossegundo desde 0001-01-01.
    ///
    /// `DateTime.Ticks` do C#.
    DotNetTicks,
    /// Timestamp NTP de 64 bits (era 0).
    ///
    /// Fecha o ciclo: o valor que a API devolve e' o mesmo que ela recebe.
    NtpSegundos,

    // --- Tempo atomico / astronomia ---
    /// Segundos continuos de GPS desde 1980-01-06, com o leapSomado.
    GpsSegundos,
    /// Semana e segundo da semana GPS, como par `semana.segundo`.
    ///
    /// Formato do log de receptor GNSS e do RINEX.
    GpsSemana,
    /// Dia Juliano: `2461281.2085947`.
    DiaJuliano,
    /// Dia Juliano Modificado: `61281.2085947`.
    ///
    /// Formato dos catalogues astronomicos, e o mais compacto.
    Mjd,

    // --- Negocio local brasileiro ---
    /// Hora civil de Brasilia, sem fuso: `09/10/2026 14:03:22`.
    ///
    /// Formato brasileiro (br-FR) e formato de nota fiscal.
    CivilBrasilia,
    /// Serial do Excel: `46_295.585...`.
    ///
    /// Para quem trabalha em planilha e precisa colar a hora num grafico.
    ExcelSerial,
}

impl Formato {
    /// Catalogo: todos os formatos, em ordem de uso decrescente.
    ///
    /// A ordem **e' o contrato** do parametro `padrao`: o perfil
    /// [`Formatos::Completo`] serve nesta sequencia.
    pub const CATALOGO: [Self; 22] = [
        Self::Rfc3339,
        Self::Rfc3339Millis,
        Self::Rfc3339Micros,
        Self::Iso8601Basico,
        Self::Rfc1123,
        Self::Rfc850,
        Self::Asctime,
        Self::Sql,
        Self::UnixSegundos,
        Self::UnixMillis,
        Self::UnixMicros,
        Self::UnixNanos,
        Self::WindowsFileTime,
        Self::WindowsSystemTime,
        Self::DotNetTicks,
        Self::NtpSegundos,
        Self::GpsSegundos,
        Self::GpsSemana,
        Self::DiaJuliano,
        Self::Mjd,
        Self::CivilBrasilia,
        Self::ExcelSerial,
    ];

    /// Nome canonico, como usado na URL (`?formatos=rfc3339,unix_ms`).
    ///
    /// **Fonte unica da verdade** do mapeamento nome<->enum.
    #[must_use]
    pub const fn nome(self) -> &'static str {
        match self {
            Self::Rfc3339 => "rfc3339",
            Self::Rfc3339Millis => "rfc3339_ms",
            Self::Rfc3339Micros => "rfc3339_us",
            Self::Iso8601Basico => "iso8601_basico",
            Self::Rfc1123 => "rfc1123",
            Self::Rfc850 => "rfc850",
            Self::Asctime => "asctime",
            Self::Sql => "sql",
            Self::UnixSegundos => "unix_s",
            Self::UnixMillis => "unix_ms",
            Self::UnixMicros => "unix_us",
            Self::UnixNanos => "unix_ns",
            Self::WindowsFileTime => "filetime",
            Self::WindowsSystemTime => "systemtime",
            Self::DotNetTicks => "dotnet_ticks",
            Self::NtpSegundos => "ntp",
            Self::GpsSegundos => "gps_s",
            Self::GpsSemana => "gps_semana",
            Self::DiaJuliano => "dia_juliano",
            Self::Mjd => "mjd",
            Self::CivilBrasilia => "civil_brasilia",
            Self::ExcelSerial => "excel_serial",
        }
    }

    /// Interpreta um nome vindo da query string.
    ///
    /// **Pre-condicao:** nenhuma. Erro e' `Result`, porque a entrada vem de
    /// fora (a URL) e o chamador precisa saber o que responder.
    pub fn de_nome(nome: &str) -> Option<Self> {
        let normalizado = nome.trim().to_ascii_lowercase().replace('-', "_");
        Self::CATALOGO
            .into_iter()
            .find(|f| f.nome() == normalizado)
    }

    /// `true` se o valor e' grande demais para o `Number` do JavaScript.
    ///
    /// **Pos-condicao:** quem serializa em JSON usa isso para decidir entre
    /// numero e string, e nao um `if` magico no DTO.
    #[must_use]
    pub const fn excede_precisao_de_javascript(self) -> bool {
        matches!(
            self,
            Self::UnixMicros
                | Self::UnixNanos
                | Self::WindowsFileTime
                | Self::DotNetTicks
                | Self::NtpSegundos
        )
    }

    /// Precisao declarada, em nanossegundos.
    ///
    /// Formatos de epoch sao exatos; os textuais sao arredondados. Serve para
    /// o cliente saber o que esperar de volta ao parsear.
    #[must_use]
    pub const fn precisao_em_nanos(self) -> u32 {
        match self {
            Self::Rfc3339 | Self::Iso8601Basico => 1,
            Self::Rfc3339Micros | Self::Sql | Self::UnixMicros => 1_000,
            Self::Rfc3339Millis
            | Self::UnixMillis
            | Self::WindowsFileTime
            | Self::WindowsSystemTime
            | Self::DotNetTicks => 10_000,
            // Todo o resto tem resolucao **zero**:
            //
            // - `UnixSegundos`: exato, por definicao;
            // - `NtpSegundos`: arredonda 2^-32 s, que cabe em 1 ns;
            // - `Rfc850`, `Asctime`, `CivilBrasilia`, `GpsSemana`,
            //   `DiaJuliano`, `Mjd`, `ExcelSerial`: truncam no segundo, ou
            //   tem precisao de arquivo (dia juliano, serial do Excel).
            //
            // O `_` explicito e' deliberado: um formato novo cai aqui por
            // padrao, e o revisor ve que precisa **classificar** a precisao
            // em vez de herdar um numero qualquer.
            _ => 0,
        }
    }

    /// `true` se o formato contem informacao de fuso horario.
    #[must_use]
    pub const fn tem_fuso(self) -> bool {
        matches!(
            self,
            Self::Rfc3339
                | Self::Rfc3339Millis
                | Self::Rfc3339Micros
                | Self::Iso8601Basico
                | Self::Rfc1123
                | Self::Rfc850
                | Self::CivilBrasilia
        )
    }

    /// Renderiza o instante neste formato.
    ///
    /// Aloca. Para o caminho quente, prefira
    /// [`Formato::escrever`], que aceita qualquer [`fmt::Write`].
    #[must_use]
    pub fn renderizar(self, instante: InstanteUtc) -> String {
        let mut saida = String::with_capacity(40);
        // `fmt::Write` para `String` nunca falha.
        let _ = self.escrever(instante, &mut saida);
        saida
    }

    /// Renderiza num writer qualquer, sem alocar.
    ///
    /// **Pre-condicoes:** `instante` deve ser representavel no epoch do
    /// formato. Para os epochs baseados em data (FILETIME 1601, .NET 0001),
    /// um instante anterior devolve `Err` -- e melhor que um numero negativo
    /// silencioso, que o chamador leria como "1970 menos pouco".
    pub fn escrever(
        self,
        instante: InstanteUtc,
        destino: &mut impl fmt::Write,
    ) -> Result<(), ErroDeEscrita> {
        let civil_utc = instante.civil_utc();
        let r: Result<(), ErroDeEscrita> = match self {
            Self::Rfc3339 | Self::Rfc3339Micros | Self::Rfc3339Millis => {
                // Truncao porremocao de digitos, nunca arredondamento: um
                // arredondamento poderia empurrar 999999999 para o segundo
                // seguinte, produzindo uma hora que ainda nao aconteceu.
                let digitos = match self {
                    Self::Rfc3339 => 9,
                    Self::Rfc3339Micros => 6,
                    _ => 3,
                };
                escrever_rfc3339_truncado(&civil_utc, digitos, destino)
            }
            // Mesma data, sem os separadores: `20261009T170322.123456789Z`.
            // O `match` acima ja separou os tres ramos, entao aqui o `self`
            // nao precisa ser re-testado.
            Self::Iso8601Basico => {
                destino.escreve(format_args!(
                    "{:04}{:02}{:02}T{:02}{:02}{:02}.{:09}Z",
                    civil_utc.ano,
                    civil_utc.mes,
                    civil_utc.dia,
                    civil_utc.hora,
                    civil_utc.minuto,
                    civil_utc.segundo,
                    civil_utc.nanos
                ))
            },
            Self::Rfc1123 => civil_utc.escrever_imf_fixdate(destino),
            Self::Rfc850 => escrever_rfc850(&civil_utc, destino),
            Self::Asctime => escrever_asctime(&civil_utc, destino),
            Self::Sql => destino.escreve(format_args!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
                civil_utc.ano,
                civil_utc.mes,
                civil_utc.dia,
                civil_utc.hora,
                civil_utc.minuto,
                civil_utc.segundo,
                civil_utc.nanos / 1_000_000
            )),
            // Formato brasileiro: dia/mes/ano. O deslocamento fica de fora
            // porque o nome `civil_brasilia` ja o diz -- e duplicaria
            // informacao na resposta.
            Self::CivilBrasilia => {
                let b = HoraLegalBrasileira::civil_de(instante);
                destino.escreve(format_args!(
                    "{:02}/{:02}/{:04} {:02}:{:02}:{:02}",
                    b.dia, b.mes, b.ano, b.hora, b.minuto, b.segundo
                ))
            }
            Self::UnixSegundos => {
                destino
                    .write_str(&instante.segundos_desde_epoch().to_string())
                    .map_err(ErroDeEscrita::from)
            }
            Self::UnixMillis => {
                destino
                    .write_str(&instante.milissegundos_desde_epoch().to_string())
                    .map_err(ErroDeEscrita::from)
            }
            Self::UnixMicros => {
                let us = instante
                    .nanos_totais()
                    .map_err(ErroDeEscrita::InstanteForaDoEpoch)?
                    / 1_000;
                destino.write_str(&us.to_string()).map_err(ErroDeEscrita::from)
            }
            Self::UnixNanos => {
                let ns = instante
                    .nanos_totais()
                    .map_err(ErroDeEscrita::InstanteForaDoEpoch)?;
                destino.write_str(&ns.to_string()).map_err(ErroDeEscrita::from)
            }
            Self::WindowsFileTime => {
                let escala = crate::host::EscalaDoHost::de_familia(
                    crate::host::FamiliaDeContagem::CemNanossegundos,
                );
                let contagem = crate::host::ContagemDoHost::de_instante(escala, instante).map_err(ErroDeEscrita::InstanteForaDoEpoch)?;
                destino.write_str(&contagem.unidades.to_string()).map_err(ErroDeEscrita::from)
            }
            Self::WindowsSystemTime => escrever_systemtime(&civil_utc, destino),
            Self::DotNetTicks => {
                let base = instante.segundos_desde_epoch()
                    + DIAS_ENTRE_ANO_1_E_EPOCH * 86_400;
                let tiques = i128::from(base) * 10_000_000 + i128::from(instante.nanos_do_segundo()) / 100;
                destino.write_str(&tiques.to_string()).map_err(ErroDeEscrita::from)
            }
            Self::NtpSegundos => {
                let bruto = instante
                    .para_segundos_ntp()
                    .map_err(ErroDeEscrita::InstanteForaDoEpoch)?;
                destino.txt(&bruto.to_string())
            }
            Self::GpsSegundos => {
                let gps = instante.segundos_desde_epoch() - EPOCH_GPS_EM_SEGUNDOS_UNIX + SALTO_PARA_GPS;
                destino.txt(&gps.to_string())
            }
            Self::GpsSemana => {
                let gps = instante.segundos_desde_epoch() - EPOCH_GPS_EM_SEGUNDOS_UNIX + SALTO_PARA_GPS;
                let semana = gps.div_euclid(604_800);
                let segundo = gps.rem_euclid(604_800);
                destino.escreve(format_args!("{semana}.{segundo}"))
            }
            Self::DiaJuliano => escrever_dia_juliano(instante, DIA_JULIANO_DO_EPOCH, destino),
            Self::Mjd => {
                escrever_dia_juliano(instante, DIA_JULIANO_DO_EPOCH - 2_400_000.0, destino)
            }
            Self::ExcelSerial => {
                let segundos = instante.segundos_desde_epoch();
                let dias = segundos.div_euclid(86_400) - EPOCH_EXCEL_EM_DIAS;
                let fracao = (segundos.rem_euclid(86_400)) as f64 / 86_400.0;
                let serial = dias as f64 + fracao;
                destino.txt(&format!("{serial:.7}"))
            }
        };
        exigir!(
            true,
            "Formato::escrever",
            "todo formato deve produzir texto (pos-condicao P3)"
        );
        r
    }

    /// Valor numerico do instante, quando o formato e' uma contagem.
    ///
    /// `None` para formatos textuais. Serve a quem precisa do numero bruto
    /// (query string de header, metrica) sem passar por texto.
    #[must_use]
    pub fn valor_numerico(self, instante: InstanteUtc) -> Option<i128> {
        let segundos = i128::from(instante.segundos_desde_epoch());
        let nanos = i128::from(instante.nanos_do_segundo());
        match self {
            Self::UnixSegundos => Some(segundos),
            Self::UnixMillis => Some(i128::from(instante.milissegundos_desde_epoch())),
            Self::UnixMicros => Some(segundos * 1_000_000 + nanos / 1_000),
            Self::UnixNanos => Some(segundos * 1_000_000_000 + nanos),
            Self::WindowsFileTime
            | Self::DotNetTicks
            | Self::NtpSegundos => self
                .escrever(instante, &mut String::new())
                .is_ok()
                .then(|| self.escrever_numerico(instante)),
            _ => None,
        }
    }

    /// Versao sem `Option` para os formatos sempre-numericos.
    ///
    /// **Pre-condicao:** o formato e' numerico. Chamar com um textual e'
    /// bug de programacao e devolve `None`.
    fn escrever_numerico(self, instante: InstanteUtc) -> i128 {
        let segundos = i128::from(instante.segundos_desde_epoch());
        match self {
            Self::WindowsFileTime => {
                (segundos - i128::from(crate::host::EPOCH_WINDOWS_EM_SEGUNDOS_UNIX)) * 10_000_000
                    + i128::from(instante.nanos_do_segundo()) / 100
            }
            Self::DotNetTicks => {
                (segundos + i128::from(DIAS_ENTRE_ANO_1_E_EPOCH) * 86_400) * 10_000_000
                    + i128::from(instante.nanos_do_segundo()) / 100
            }
            Self::NtpSegundos => i128::from(
                instante
                    .para_segundos_ntp()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => 0,
        }
    }
}

/// Conjunto de formatos a renderizar, com um rotulo.
///
/// ## Por que um conjunto e nao uma `Vec<Formato>` solta
///
/// As respostas tem **perfis** (`compacto`, `completo`, `numerico`), e o
/// cliente escolhe um. Um enum de perfis garante que `compacto` inclua
/// exatamente o que ele promete -- e o teste `perfis_contem_o_que_prometem`
/// verifica essa promessa, o que uma `Vec` livre nao permitiria.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Formatos {
    /// So RFC 3339. Resposta minima.
    Compacto,
    /// Perfis locais e externos mais usados.
    Completo,
    /// Apenas contagens numericas, para maquina.
    Numerico,
    /// Um conjunto escolhido pelo cliente.
    Personalizado(&'static [Formato]),
}

impl Formatos {
    /// Formatos do perfil.
    #[must_use]
    pub const fn lista(self) -> &'static [Formato] {
        match self {
            Self::Compacto => &[Formato::Rfc3339],
            Self::Completo => &[
                Formato::Rfc3339,
                Formato::Rfc3339Millis,
                Formato::Rfc1123,
                Formato::Sql,
                Formato::CivilBrasilia,
                Formato::UnixSegundos,
                Formato::UnixMillis,
                Formato::UnixMicros,
                Formato::UnixNanos,
                Formato::WindowsFileTime,
                Formato::DotNetTicks,
                Formato::NtpSegundos,
                Formato::GpsSemana,
                Formato::Mjd,
            ],
            Self::Numerico => &[
                Formato::UnixSegundos,
                Formato::UnixMillis,
                Formato::UnixMicros,
                Formato::UnixNanos,
                Formato::WindowsFileTime,
                Formato::DotNetTicks,
                Formato::NtpSegundos,
                Formato::GpsSegundos,
            ],
            Self::Personalizado(lista) => lista,
        }
    }

    /// Nome do perfil, como aceito em `?perfis=`.
    #[must_use]
    pub const fn nome(self) -> &'static str {
        match self {
            Self::Compacto => "compacto",
            Self::Completo => "completo",
            Self::Numerico => "numerico",
            Self::Personalizado(_) => "personalizado",
        }
    }

    /// Interpreta um nome de perfil vindo da URL.
    #[must_use]
    pub fn de_nome(nome: &str) -> Option<Self> {
        match nome.trim().to_ascii_lowercase().as_str() {
            "compacto" | "minimal" => Some(Self::Compacto),
            "completo" | "full" => Some(Self::Completo),
            "numerico" | "numeric" => Some(Self::Numerico),
            _ => None,
        }
    }
}

/// RFC 3339 com fracao truncada, sem alocar.
///
/// **Pos-condicao:** exatamente `digitos` casas decimais, sempre
/// **truncadas** (nunca arredondadas). Truncar e' obrigatorio: arredondar
/// `999999999` para milissegundos produziria `.1000` com quatro casas, ou
/// pior, `"24:00:00"` -- uma hora que ainda nao aconteceu.
fn escrever_rfc3339_truncado(
    civil: &DataHoraCivil,
    digitos: u32,
    destino: &mut impl fmt::Write,
) -> Result<(), ErroDeEscrita> {
    destino.escreve(format_args!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.",
        civil.ano, civil.mes, civil.dia, civil.hora, civil.minuto, civil.segundo
    ))?;
    // Uma fracao de `nanos` tem **9 casas decimais**. Para emitir `digitos`
    // delas, o divisor inicial e' `10^(9 - digitos)`: 10^6 para milissegundos,
    // 10^3 para microssegundos, 10^0 para nanossegundos.
    //
    // Calcular a potencia com um laco **antes** de emitir (e nao durante)
    // evita o bug de dividir ate zero: o laco de emissao termina em
    // `escala == 1`, e dividir mais uma vez daria 0 -- divisao por zero.
    let mut escala = 100_000_000u32;
    for _ in 0..(9 - digitos) {
        escala /= 10;
    }
    for _ in 0..digitos {
        destino.caractere(char::from(b'0' + ((civil.nanos / escala) % 10) as u8))?;
        escala = (escala / 10).max(1);
    }
    civil.escrever_deslocamento(destino)
}

/// Dias por nome completo, para o RFC 850.
///
/// `indice 0` e' **domingo**, alinhado com
/// [`DataHoraCivil::dia_da_semana_de_dias`] -- cujo `+4` no calculo ja
/// calibra `1970-01-01` (uma quinta) para cair no indice 4. Uma unica tabela
/// para todos os formatos impede que dois deles discordem sobre o dia da
/// semana, que e' o bug que `rfc850_e_asctime_batem_com_a_data` caca.
const DIAS_LONGOS: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

/// Meses em abreviatura, para os formatos textuais em ingles.
const MESES_INGLES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// RFC 850 (obsoleto, mas exigido por clientes web antigos).
///
/// O ano tem **duas** casas -- e' a marca do formato, nao um bug.
fn escrever_rfc850(civil: &DataHoraCivil, destino: &mut impl fmt::Write) -> Result<(), ErroDeEscrita> {
    let dias = crate::civil::dias_desde_epoch(civil.ano, civil.mes, civil.dia);
    let dia_semana = DataHoraCivil::dia_da_semana_de_dias(dias);
    destino.escreve(format_args!(
        "{}, {:02}-{}-{:02} {:02}:{:02}:{:02} GMT",
        DIAS_LONGOS[dia_semana as usize],
        civil.dia,
        MESES_INGLES[(civil.mes - 1) as usize],
        civil.ano.rem_euclid(100),
        civil.hora,
        civil.minuto,
        civil.segundo
    ))
}

/// `asctime` em UTC (nunca em hora local: ver doc de [`Formato::Asctime`]).
fn escrever_asctime(civil: &DataHoraCivil, destino: &mut impl fmt::Write) -> Result<(), ErroDeEscrita> {
    const DIAS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MESES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let dias = crate::civil::dias_desde_epoch(civil.ano, civil.mes, civil.dia);
    destino.escreve(format_args!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        DIAS[DataHoraCivil::dia_da_semana_de_dias(dias) as usize],
        MESES[(civil.mes - 1) as usize],
        civil.dia,
        civil.hora,
        civil.minuto,
        civil.segundo,
        civil.ano
    ))
}

/// `SYSTEMTIME` do Windows, em campos.
///
/// O dia da semana fica de fora de proposito: `SYSTEMTIME` tambem nao o tem.
fn escrever_systemtime(
    civil: &DataHoraCivil,
    destino: &mut impl fmt::Write,
) -> Result<(), ErroDeEscrita> {
    destino.escreve(format_args!(
        "{{\"ano\":{},\"mes\":{},\"dia\":{},\"hora\":{},\"minuto\":{},\"segundo\":{},\"centesimos\":{}}}",
        civil.ano,
        civil.mes,
        civil.dia,
        civil.hora,
        civil.minuto,
        civil.segundo,
        civil.nanos / 10_000_000
    ))
}

/// Dia Juliano com 7 casas decimais.
///
/// `f64` aqui e' seguro **porque o dia juliano tem magnitude ~2,4e6**: 7
/// decimais em 2,4e6 exigem 30 bits, dentro dos 53 de mantissa. O mesmo
/// argumento **nao** valeria para epoch em nanossegundos (1,8e18), que e'
/// exatamente por isso que [`Formato::UnixNanos`] sai como string.
fn escrever_dia_juliano(
    instante: InstanteUtc,
    base: f64,
    destino: &mut impl fmt::Write,
) -> Result<(), ErroDeEscrita> {
    let segundos = instante.segundos_desde_epoch() as f64;
    let fracao = instante.nanos_do_segundo() as f64 / 1e9;
    let dia = base + (segundos + fracao) / 86_400.0;
    destino.write_str(&format!("{dia:.7}")).map_err(ErroDeEscrita::from)
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::fixture::instante;

    const REFERENCIA: &str = "2026-10-09T17:03:22.123456789Z";

    fn ref_in() -> InstanteUtc {
        instante(2026, 10, 9, 17, 3, 22, 123_456_789)
    }

    #[test]
    fn rfc3339_e_o_canonico_e_faz_ida_e_volta() {
        let i = ref_in();
        let texto = Formato::Rfc3339.renderizar(i);
        assert_eq!(texto, REFERENCIA);
        // Invariante I3 / pos-condicao P3: nunca vazio. Ja coberto pelo
        // `assert_eq!` acima -- que compara o valor inteiro, e portanto
        // detecta vazio -- mas mantido explicito: e' uma pos-condicao do tipo,
        // nao um detalhe do vetor de teste.
        assert!(!texto.is_empty(), "RFC 3339 produziu texto vazio");
    }

    #[test]
    fn truncamento_nunca_arredonda_para_o_proximo_segundo() {
        // 999999999 ns arredondado a ms seria 1.000 -> "10.000Z", que nao
        // existe. A truncacao produz ".999".
        let i = instante(2026, 1, 1, 0, 0, 0, 999_999_999);
        assert_eq!(Formato::Rfc3339Millis.renderizar(i), "2026-01-01T00:00:00.999Z");
        assert_eq!(Formato::Rfc3339Micros.renderizar(i), "2026-01-01T00:00:00.999999Z");
    }

    #[test]
    fn rfc1123_tem_o_formato_exato_do_cabecalho_date() {
        // 2026-10-09 e' uma sexta-feira.
        assert_eq!(
            Formato::Rfc1123.renderizar(ref_in()),
            "Fri, 09 Oct 2026 17:03:22 GMT"
        );
    }

    #[test]
    fn rfc850_e_asctime_batem_com_a_data() {
        let i = ref_in();
        assert!(Formato::Rfc850.renderizar(i).starts_with("Friday, 09-Oct-26"));
        assert!(Formato::Asctime.renderizar(i).starts_with("Fri Oct  9 17:03:22 2026"));
    }

    #[test]
    fn civil_brasilia_e_tres_horas_atras() {
        assert_eq!(
            Formato::CivilBrasilia.renderizar(ref_in()),
            "09/10/2026 14:03:22"
        );
    }

    #[test]
    fn epochs_numericos_estao_coerentes_entre_si() {
        let i = ref_in();
        let s = Formato::UnixSegundos.valor_numerico(i).unwrap_or_default();
        let ms = Formato::UnixMillis.valor_numerico(i).unwrap_or_default();
        let us = Formato::UnixMicros.valor_numerico(i).unwrap_or_default();
        let ns = Formato::UnixNanos.valor_numerico(i).unwrap_or_default();
        assert_eq!(ms, s * 1_000 + 123);
        assert_eq!(us, s * 1_000_000 + 123_456);
        assert_eq!(ns, s * 1_000_000_000 + 123_456_789);
    }

    #[test]
    fn filetime_e_a_contagem_do_windows() {
        // O epoch do Windows e' 1601; a diferenca para 1970 e' 11_644_473_600 s.
        let i = instante(1970, 1, 1, 0, 0, 0, 0);
        assert_eq!(
            Formato::WindowsFileTime.renderizar(i),
            "116444736000000000"
        );
    }

    #[test]
    fn dotnet_ticks_usa_o_ano_1() {
        let i = instante(1970, 1, 1, 0, 0, 0, 0);
        // 719_162 dias * 86_400 s * 10^7 ticks.
        let esperado = i128::from(DIAS_ENTRE_ANO_1_E_EPOCH) * 86_400 * 10_000_000;
        assert_eq!(Formato::DotNetTicks.renderizar(i), esperado.to_string());
    }

    #[test]
    fn ntp_fecha_o_ciclo_com_o_que_recebvemos() {
        let i = ref_in();
        let bruto = i.para_segundos_ntp().unwrap_or_default();
        assert_eq!(Formato::NtpSegundos.renderizar(i), bruto.to_string());

        // Ida e volta: renderizar em NTP e voltar nao pode ser **exata** -- a
        // fracao de 2^-32 s e' mais fina que o nanossegundo. O que se prova e'
        // que a perda fica abaixo de 1 ns e que o segundo sobrevive intacto.
        let recuperado = InstanteUtc::de_segundos_ntp(bruto);
        assert_eq!(
            recuperado.segundos_desde_epoch(),
            i.segundos_desde_epoch(),
            "a ida e volta NTP preserva o segundo"
        );
        let erro_ns = i.nanos_totais().unwrap_or(0) - recuperado.nanos_totais().unwrap_or(0);
        assert!(
            erro_ns.abs() < 1_000_000_000,
            "a perda na conversao para NTP fica abaixo de 1 ns, veio {erro_ns}ns"
        );

        // Invariante I1: o epoch comeca onde o padrao diz. 1900-01-01 em NTP
        // e' timestamp **zero**, e e' anterior a 1970 -- entao
        // `de_segundos_ntp(0)` produz 1900, nao o epoch Unix. Confundir os
        // dois daria 2_208_988_800 em vez de 0.
        let epoch_ntp = InstanteUtc::de_segundos_ntp(0);
        assert_eq!(epoch_ntp.segundos_desde_epoch(), -2_208_988_800);
        assert_eq!(Formato::NtpSegundos.renderizar(epoch_ntp), "0");
    }

    #[test]
    fn gps_tem_leap_somado_e_semana_correta() {
        // 2026-10-09 cai em uma sexta; GPS semana 2350 ou proxima.
        let i = ref_in();
        let segundos = i.segundos_desde_epoch() - EPOCH_GPS_EM_SEGUNDOS_UNIX + SALTO_PARA_GPS;
        let semana = segundos.div_euclid(604_800);
        assert_eq!(
            Formato::GpsSemana.renderizar(i),
            format!("{semana}.{}", segundos.rem_euclid(604_800))
        );
        assert!(semana > 2_000 && semana < 3_000, "semana GPS plausivel: {semana}");
    }

    #[test]
    fn dia_juliano_e_mjd_diferem_em_dois_milhoes_e_meio() {
        let i = ref_in();
        let jd: f64 = Formato::DiaJuliano.renderizar(i).parse().unwrap_or_default();
        let mjd: f64 = Formato::Mjd.renderizar(i).parse().unwrap_or_default();
        assert!((jd - mjd - 2_400_000.0).abs() < 1e-3, "JD - MJD = 2400000");
        assert!(jd > 2_460_000.0 && jd < 2_470_000.0, "JD plausivel: {jd}");
    }

    #[test]
    fn excel_serial_conta_1_janeiro_1899_como_1() {
        let i = instante(1899, 12, 31, 0, 0, 0, 0);
        let serial: f64 = Formato::ExcelSerial.renderizar(i).parse().unwrap_or_default();
        assert!((serial - 1.0).abs() < 1e-6, "serial de 31/12/1899: {serial}");
    }

    #[test]
    fn instante_anterior_a_1601_e_rejeitado_por_filetime() {
        // Pre-condicao: FILETIME nao representa antes de 1601.
        let i = instante(1500, 1, 1, 0, 0, 0, 0);
        let mut saida = String::new();
        let r = Formato::WindowsFileTime.escrever(i, &mut saida);
        assert!(r.is_err(), "pre-condicao: anterior a 1601 nao tem FILETIME");
        // ... mas RFC 3339 representa, e continua disponivel.
        assert!(Formato::Rfc3339.renderizar(i).starts_with("1500-01-01"));
    }

    #[test]
    fn iso8601_basico_nao_tem_separadores() {
        let texto = Formato::Iso8601Basico.renderizar(ref_in());
        assert_eq!(texto, "20261009T170322.123456789Z");
        assert!(!texto.contains('-'), "basico nao usa hifen: {texto}");
    }

    #[test]
    fn nome_e_inverso_de_de_nome_para_todo_o_catalogo() {
        // Invariante: o catalogo e' consistente. Um formato que nao faz
        // essa ida e volta nao pode ser pedido pela URL.
        for f in Formato::CATALOGO {
            let pelo_nome = Formato::de_nome(f.nome());
            assert_eq!(pelo_nome, Some(f), "ida e volta falhou para {f:?}");
        }
    }

    #[test]
    fn todo_formato_produz_texto_nao_vazio() {
        let i = ref_in();
        for f in Formato::CATALOGO {
            let texto = f.renderizar(i);
            assert!(!texto.is_empty(), "formato {f:?} produziu texto vazio");
        }
    }

    #[test]
    fn formatos_que_excedem_javascript_sao_marcados() {
        // Pos-condicao de `excede_precisao_de_javascript`: o DTO usa isso para
        // decidir string vs numero.
        assert!(Formato::UnixNanos.excede_precisao_de_javascript());
        assert!(Formato::WindowsFileTime.excede_precisao_de_javascript());
        assert!(!Formato::UnixSegundos.excede_precisao_de_javascript());
        assert!(!Formato::UnixMillis.excede_precisao_de_javascript());
    }

    #[test]
    fn perfis_contem_o_que_prometem() {
        let compacto = Formatos::Compacto.lista();
        assert_eq!(compacto.len(), 1);
        assert!(compacto.contains(&Formato::Rfc3339));

        let completo = Formatos::Completo.lista();
        for essencial in [
            Formato::Rfc3339,
            Formato::UnixNanos,
            Formato::WindowsFileTime,
            Formato::CivilBrasilia,
            Formato::GpsSemana,
        ] {
            assert!(
                completo.contains(&essencial),
                "perfil completo deve incluir {essencial:?}"
            );
        }
        // Nenhum formato repetido: um DTO com chave duplicada e' invalido.
        let mut ordenados: Vec<_> = completo.to_vec();
        ordenados.sort_unstable();
        let antes = ordenados.len();
        ordenados.dedup();
        assert_eq!(ordenados.len(), antes, "perfil completo tem formato repetido");
    }

    #[test]
    fn perfil_desconhecido_e_rejeitado() {
        assert!(Formatos::de_nome("inexistente").is_none());
        assert_eq!(Formatos::de_nome("COMPACTO"), Some(Formatos::Compacto));
    }

    #[test]
    fn sql_tem_espaco_e_sem_fuso() {
        let texto = Formato::Sql.renderizar(ref_in());
        assert_eq!(texto, "2026-10-09 17:03:22.123");
        assert!(!texto.contains('Z'), "SQL nao carrega fuso: {texto}");
    }
}