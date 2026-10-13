//! Conversao instante <-> calendario civil, sem dependencias externas.
//!
//! A aritmetica e' a de Howard Hinnant (`chrono`-compatible): dias para
//! (ano, mes, dia) via o "dia civil a partir de Zeller deslocado", que vale
//! para datas negativas -- essencial, porque o epoch NTP e' 1900.
//!
//! Este e' um dos pontos de costura do sistema: representacao numerica interna
//! vira texto. Por isso tem teste dedicado de vetores conhecidos, e nao uma
//! formatacao inline na camada HTTP.

#![allow(
    // Os indices das tabelas sao mascarados (`& MASCARA_DE_DIA`,
    // `& MASCARA_DE_MES`) ou vem de um `match` com faixa exata. Um `.get()`
    // devolveria `Option` e forcaria o chamador a tratar um caso que a
    // mascara ja torna impossivel -- trocando um `panic!` improvavel por um
    // tratamento de impossibilidade.
    clippy::indexing_slicing,
    // As conversoes `i64 -> u8` em `de_dias_desde_epoch` sao `try_into` com
    // `unwrap_or(0)`: o algoritmo garante [1..=31] e [1..=12], e o fallback
    // mantem a funcao **total**, como a assinatura promete.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
)]
use std::fmt;

use crate::instante::Escrita;

/// Calendario civil (proleatico gregoriano) com deslocamento de fuso.
///
/// Tipo fechado: um instante UTC nao tem `Option<fuso>`. Quem nao tem fuso usa
/// [`DataHoraCivil::de_instante`], que produz deslocamento zero e renderiza
/// `Z`.
///
/// **Invariante:** `1 <= mes <= 12`, `1 <= dia <= 31`, `0 <= hora <= 23`,
/// `0 <= minuto <= 59`, `0 <= segundo <= 59`, `nanos < 1_000_000_000`,
/// `DESLOCAMENTO_MINIMO <= deslocamento_segundos <= DESLOCAMENTO_MAXIMO`.
///
/// ### Por que os campos sao publicos
///
/// Nao ha construtor: qualquer `DataHoraCivil { .. }` com campos publicos e'
/// construivel. A invariante acima e' garantida pelo **fato de este tipo so
/// ser produzido por metodos que calculam de um `InstanteUtc`**, e nao por
/// um `Default` publico. E' a mesma razao pela qual `InstanteUtc` tem campos
/// privados: aqui o preco da privacidade (construtor + getters) nao se paga,
/// porque nao existe entrada manual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataHoraCivil {
    /// Ano proleatico gregoriano (pode ser negativo).
    pub ano: i64,
    /// Mes, 1..=12.
    pub mes: u8,
    /// Dia do mes, 1..=31.
    pub dia: u8,
    /// Hora, 0..=23.
    pub hora: u8,
    /// Minuto, 0..=59.
    pub minuto: u8,
    /// Segundo, 0..=59.
    ///
    /// Nunca 60: segundos intercalares sao ignorados por decisao
    /// documentada. A RFC 3339 permite `60`; o ON nao emite, e aceitar
    /// exigiria um estado adicional em todo o caminho de formatacao.
    pub segundo: u8,
    /// Nanosssegundo, `0..=999_999_999`.
    pub nanos: u32,
    /// Deslocamento em segundos do fuso, `-86_399..=86_399`.
    pub deslocamento_segundos: i32,
}

impl DataHoraCivil {
    /// Menor deslocamento representavel (-23:59:59). Abaixo disso `DataHoraCivil`
    /// deixaria de ser um fuso real.
    pub const DESLOCAMENTO_MINIMO: i32 = -86_399;
    /// Maior deslocamento representavel.
    pub const DESLOCAMENTO_MAXIMO: i32 = 86_399;

    /// Converte dias desde 1970-01-01 em (ano, mes, dia).
    ///
    /// **Pre-condicao:** nenhuma -- todo `i64` e' um dia valido.
    #[must_use]
    pub fn de_dias_desde_epoch(dias: i64) -> (i64, u8, u8) {
        // Hinnant: desloca a era para que o ano comeca em marco, e a divisao
        // dos 400 anos caia inteira (sem ajuste negativo).
        let deslocado = dias + 719_468;
        let era = deslocado.div_euclid(146_097);
        let dia_na_era = deslocado.rem_euclid(146_097); // [0, 146_096]
        let ano_no_era =
            (dia_na_era - dia_na_era / 1_460 + dia_na_era / 36_524 - dia_na_era / 146_096) / 365;
        let ano = ano_no_era + era * 400;
        let dia_no_ano = dia_na_era - (365 * ano_no_era + ano_no_era / 4 - ano_no_era / 100);
        let mp = (5 * dia_no_ano + 2) / 153; // [0, 11], marco = 0
        let dia = (dia_no_ano - (153 * mp + 2) / 5 + 1) as u8;
        let mes = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        (if mes <= 2 { ano + 1 } else { ano }, mes, dia)
    }

    /// Dia da semana de um dia desde 1970-01-01: `0 = domingo` .. `6 = sabado`.
    ///
    /// **Pos-condicao:** `dia_da_semana_de_dias(0) == 4` -- 1970-01-01 foi
    /// uma quinta-feira. O `+4` e' a calibracao desse fato, e e' a unica
    /// razao de o deslocamento existir; ele esta aqui, e nao espalhado em cada
    /// tabela de nomes.
    #[must_use]
    pub const fn dia_da_semana_de_dias(dias: i64) -> u8 {
        (dias + 4).rem_euclid(7) as u8
    }

    /// Dia da semana desta data civil.
    #[must_use]
    pub fn dia_da_semana(&self) -> u8 {
        Self::dia_da_semana_de_dias(dias_desde_epoch(self.ano, self.mes, self.dia))
    }

    /// Escreve RFC 3339 com precisao nanometrica fixa.
    ///
    /// Nao aloca: aceita qualquer [`fmt::Write`], permitindo que a camada HTTP
    /// renderize direto num buffer reaproveitado -- requisito de latencia.
    pub fn escrever_rfc3339(
        &self,
        destino: &mut impl fmt::Write,
    ) -> Result<(), crate::instante::ErroDeEscrita> {
        destino.escreve(format_args!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}",
            self.ano, self.mes, self.dia, self.hora, self.minuto, self.segundo, self.nanos
        ))?;
        self.escrever_deslocamento(destino)
    }

    /// Sufixo de fuso RFC 3339: `Z` para zero, `+HH:MM` / `-HH:MM` caso contrario.
    ///
    /// **Pre-condicao:** `deslocamento_segundos` dentro de
    /// [`Self::DESLOCAMENTO_MINIMO`]..[`Self::DESLOCAMENTO_MAXIMO`].
    pub fn escrever_deslocamento(
        &self,
        destino: &mut impl fmt::Write,
    ) -> Result<(), crate::instante::ErroDeEscrita> {
        // `match` no sinal: os tres casos (nulo, negativo, positivo) sao
        // exaustivos, e um `if` aninhado daria a mesma resposta com uma
        // combinacao a menos para verificar.
        let absoluto = self.deslocamento_segundos.unsigned_abs();
        match self.deslocamento_segundos {
            0 => destino.caractere('Z'),
            negativo if negativo < 0 => destino.escreve(format_args!(
                "-{:02}:{:02}",
                absoluto / 3_600,
                (absoluto % 3_600) / 60
            )),
            _ => destino.escreve(format_args!(
                "+{:02}:{:02}",
                absoluto / 3_600,
                (absoluto % 3_600) / 60
            )),
        }
    }

    /// Escreve RFC 3339 em **portugues do Brasil**.
    ///
    /// ## Por que existe, e por que `escrever_rfc3339` nao faz isto
    ///
    /// RFC 3339 e' um padrao **internacional**: o dia vem primeiro, o
    /// separador e' `T`, e o fuso e' `Z` ou `±HH:MM`. Nao ha espaco para
    /// `sexta-feira`. Quem precisa do nome do dia em portugues (uma tela de
    /// operacao, um relatorio, um log legivel) usa este metodo; quem precisa
    /// interoperabilidade usa [`Self::escrever_rfc3339`].
    ///
    /// Concisao de formato: `sexta-feira, 09/10/2026 17:03:22 UTC`.
    pub fn escrever_extenso_pt_br(
        &self,
        destino: &mut impl fmt::Write,
    ) -> Result<(), crate::instante::ErroDeEscrita> {
        destino.escreve(format_args!(
            "{}, {:02}/{:02}/{:04} {:02}:{:02}:{:02} UTC",
            nome_do_dia_pt_br(self.dia_da_semana()),
            self.dia,
            self.mes,
            self.ano,
            self.hora,
            self.minuto,
            self.segundo
        ))
    }

    /// Escreve a data por extenso em portugues: `nove de outubro de dois mil e
    /// vinte e seis`.
    ///
    /// Para documento e para nota fiscal -- nenhum dos dois aceita numero.
    ///
    /// **Pre-condicao:** ano em 1000..=2999, coberto por
    /// [`MILHARES_PT_BR`]. Acima disso a saida seria `mil` seguida de numero
    /// -- errado, e silenciosamente. A API HTTP deve recusar o pedido antes
    /// de chegar aqui; ver `docs/API.md`.
    pub fn escrever_data_extenso_pt_br(
        &self,
        destino: &mut impl fmt::Write,
    ) -> Result<(), crate::instante::ErroDeEscrita> {
        let ano = u32::try_from(self.ano).unwrap_or_default();
        // Indice = `ano / 1000 - 1`: a tabela comeca em "mil", e 2026 tem
        // milhar = 2, logo indice 1 = "dois mil".
        let milhar = MILHARES_PT_BR
            [(usize::try_from(ano / 1000).unwrap_or(1)).saturating_sub(1) % 3];
        let resto = ano % 1000;
        // O `match` cobre os dois casos: ano redondo ("dois mil") e ano com
        // resto ("dois mil e vinte e seis"). Sem ele, 2000 sairia "dois mil
        // e zero".
        let dia_por_extenso = numero_por_extenso_pt_br(u32::from(self.dia));
        let mes_por_extenso = nome_do_mes_pt_br(self.mes);
        // Ano redondo nao leva " e <resto>". Dois casos, um `if` --
        // um `match` aqui so adicionaria ruido.
        if resto == 0 {
            return destino.escreve(format_args!(
                "{dia_por_extenso} de {mes_por_extenso} de {milhar}"
            ));
        }
        let resto_por_extenso = numero_por_extenso_pt_br(resto);
        destino.escreve(format_args!(
            "{dia_por_extenso} de {mes_por_extenso} de {milhar} e {resto_por_extenso}"
        ))
    }

    /// Escreve IMF-fixdate, o formato *obrigatorio* do cabecalho HTTP `Date`
    /// (RFC 9110). Em UTC, sempre, com `GMT`.
    ///
    /// Separate de `escrever_rfc3339` de proposito: sao formatos de dominios
    /// diferentes (API vs. HTTP) e um objeto so pode ter um `Display`.
    pub fn escrever_imf_fixdate(
        &self,
        destino: &mut impl fmt::Write,
    ) -> Result<(), crate::instante::ErroDeEscrita> {
        // O cabecalho e' sempre UTC, entao o dia da semana vem do dia UTC --
        // por isso o deslocamento deste objeto e' deliberadamente ignorado
        // aqui. Um bug aqui so apareceria em respostas com fuso nao-UTC, que
        // nao existem: `escrever_imf_fixdate` so e' chamado em UTC.
        destino.txt(&format!(
            "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
            DIAS_ABREVIADOS_EN[self.dia_da_semana() as usize],
            self.dia,
            MESES_ABREVIADOS_EN[self.mes.saturating_sub(1) as usize],
            self.ano,
            self.hora,
            self.minuto,
            self.segundo
        ))
    }
}

/// Dias da semana em abreviatura inglesa, para o cabecalho HTTP `Date`.
///
/// A RFC 9110 **exige** os nomes em ingles nesse cabecalho, mesmo em resposta
/// a um cliente brasileiro. `terça-feira` seria um `Date` invalido.
const DIAS_ABREVIADOS_EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Meses em abreviatura inglesa, para o cabecalho HTTP `Date`.
///
/// Mesma razao: o padrao HTTP e' em ingles.
const MESES_ABREVIADOS_EN: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Dias da semana, por extenso, em portugues do Brasil.
///
/// `indice 0 = domingo`, alinhado com [`DataHoraCivil::dia_da_semana`].
pub const DIAS_EXTENSO_PT_BR: [&str; 7] = [
    "domingo",
    "segunda-feira",
    "terça-feira",
    "quarta-feira",
    "quinta-feira",
    "sexta-feira",
    "sábado",
];

/// Meses, por extenso, em portugues do Brasil.
///
/// `indice 0 = janeiro`.
pub const MESES_EXTENSO_PT_BR: [&str; 12] = [
    "janeiro",
    "fevereiro",
    "março",
    "abril",
    "maio",
    "junho",
    "julho",
    "agosto",
    "setembro",
    "outubro",
    "novembro",
    "dezembro",
];

/// Meses em abreviatura, em portugues do Brasil (usados no log e no CSV).
pub const MESES_ABREVIADOS_PT_BR: [&str; 12] = [
    "jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez",
];

/// Dias da semana em abreviatura, em portugues do Brasil.
pub const DIAS_ABREVIADOS_PT_BR: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];

/// Anos por extenso, em portugues do Brasil, de 1000 a 2999.
///
/// ## Por que uma tabela e nao um algoritmo
///
/// "mil novecentos e noventa e seis" exige composicao com regra de
/// concordancia complexa. Uma tabela de 2000 entradas custaria ~60 KB no
/// binario; um algoritmo seria um projeto paralelo ao dominio.
///
/// A solucao pragmaticamente correta e **limitada**: anos ate 2999, com a
/// composicao feita no [`DataHoraCivil::escrever_data_extenso_pt_br`], que e'
/// onde a formatacao ja acontece.
pub const MILHARES_PT_BR: [&str; 3] = ["mil", "dois mil", "três mil"];

/// Nome do dia da semana em portugues.
///
/// **Total**: todo `0..=6` produz um nome.
#[must_use]
pub fn nome_do_dia_pt_br(indice: u8) -> &'static str {
    DIAS_EXTENSO_PT_BR[usize::from(indice) & MASCARA_DE_DIA]
}

/// Mascara de indice para as tabelas de 7 dias.
const MASCARA_DE_DIA: usize = 7;

/// Mascara de indice para as tabelas de 12 meses.
const MASCARA_DE_MES: usize = 11;

/// Nome do mes em portugues.
///
/// **Total**: todo `1..=12` produz um nome; indices fora do intervalo sao
/// normalizados por mascara, e o valor normalizado e' observavel.
#[must_use]
pub fn nome_do_mes_pt_br(indice: u8) -> &'static str {
    // `MASCARA_DE_MES` vale 11 (= 12 - 1). Nomeada, porque `& 11` num
    // indice de tabela e' um numero magico que ninguem revisa.
    MESES_EXTENSO_PT_BR[(usize::from(indice).wrapping_sub(1)) & MASCARA_DE_MES]
}

/// Centenas, por extenso, em portugues do Brasil (`100`..`900`).
const CENTENAS_PT_BR: [&str; 10] = [
    "", "cento", "duzentos", "trezentos", "quatrocentos", "quinhentos", "seiscentos", "setecentos",
    "oitocentos", "novecentos",
];

/// Numero cardinal por extenso, de 0 a 999, em portugues.
///
/// ### Por que `999` e nao `31`
///
/// Um dia do mes e' 1..=31, mas o **ano** tem a parte `resto % 1000`, e e'
/// essa parte que precisa do mesmo conjunto. Um unico numero por extenso
/// para os dois usos evita duas tabelas que poderiam divergir em
/// "dezesseis" vs "dezesseis".
///
/// ### A composicao
///
/// ```text
/// 926 = 9 centenas + 2 dezenas + 6 unidades
///     = "novecentos" + " e vinte" + " e seis"
/// ```
///
/// O `e` de ligacao entra **uma vez**, entre as partes nao-nulas, e nao
/// entre todas -- "novecentos e vinte e seis", nao "novecentos e e vinte e
/// seis".
#[must_use]
pub fn numero_por_extenso_pt_br(numero: u32) -> String {
    const UNIDADES: [&str; 10] = [
        "zero", "um", "dois", "três", "quatro", "cinco", "seis", "sete", "oito", "nove",
    ];
    const DEZENAS: [&str; 10] = [
        "", "dez", "vinte", "trinta", "quarenta", "cinquenta", "sessenta", "setenta", "oitenta",
        "noventa",
    ];
    /// Formas irregulares de 10 a 19.
    ///
    /// **Indice = `numero - 10`.** "Dezesseis" ocupa o indice 6, e nao o
    /// resto `16 % 10 == 6` -- que daria "seis". A tabela e' assim para
    /// que o `match` leia `10..=19 => IRREGULARES[numero - 10]`, sem
    /// aritmetica que possa sair errada silenciosamente.
    const IRREGULARES_10_19: [&str; 10] = [
        "dez", "onze", "doze", "treze", "catorze", "quinze", "dezesseis",
        "dezessete", "dezoito", "dezenove",
    ];

    match numero {
        // 0..=9: as unidades sao a propria palavra.
        0..=9 => UNIDADES[numero as usize].to_owned(),
        // 10..=19: "dez", "onze" .. "dezenove" tem forma propria -- "dez e
        // nove" seria errado.
        10..=19 => IRREGULARES_10_19[(numero - 10) as usize].to_owned(),
        // 20..=99: dezena + unidade, com "e" se houver unidade.
        20..=99 => match numero % 10 {
            0 => DEZENAS[(numero / 10) as usize].to_owned(),
            unidade => format!(
                "{} e {}",
                DEZENAS[(numero / 10) as usize],
                UNIDADES[unidade as usize]
            ),
        },
        // 100..=999: centena + resto, com "e" se houver resto.
        100..=999 => {
            let resto = numero % 100;
            // `100` e' "cem", nao "cento": a unica excecao da regra, e e' um
            // caso especial do padrao, nao um erro.
            let centena = CENTENAS_PT_BR[(numero / 100) as usize];
            match (numero / 100, resto) {
                // 100 exato: "cem", nunca "cento".
                (1, 0) => "cem".to_owned(),
                (_, 0) => centena.to_owned(),
                _ => format!("{centena} e {}", numero_por_extenso_pt_br(resto)),
            }
        }
        // Fora de 0..=999: mascarado, para nao entrar em panic de indice.
        _ => numero.to_string(),
    }
}

/// Dias entre 1970-01-01 e (ano, mes, dia). Inverso de
/// [`DataHoraCivil::de_dias_desde_epoch`].
///
/// **Invariante (I2):** para todo dia `d`,
/// `DataHoraCivil::de_dias_desde_epoch(dias_desde_epoch(d)) == d`. O teste
/// `ida_e_volta_entre_dias` verifica em uma amostra de Known reference days.
#[must_use]
pub const fn dias_desde_epoch(ano: i64, mes: u8, dia: u8) -> i64 {
    let a = ano - if mes <= 2 { 1 } else { 0 };
    let era = a.div_euclid(400);
    let ano_no_era = a - era * 400;
    // `as i64` em vez de `From::from`: o trait `From` nao e' `const` ate o stable
    // atual, e esta funcao precisa ser `const` (ver `dia_da_semana`). A
    // conversao `u8 -> i64` e' sempre exata e preserva sinal, entao o `as`
    // nao perde informacao -- e a linha abaixo documenta isso.
    let mes_i64 = mes as i64;
    let dia_i64 = dia as i64;
    let mes_no_era = if mes_i64 > 2 { mes_i64 - 3 } else { mes_i64 + 9 };
    let dia_do_ano = (153 * mes_no_era + 2) / 5 + dia_i64 - 1;
    let dia_da_era = ano_no_era * 365 + ano_no_era / 4 - ano_no_era / 100 + dia_do_ano;
    era * 146_097 + dia_da_era - 719_468
}

impl fmt::Display for DataHoraCivil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Display` nao pode devolver `ErroDeEscrita` (o trait exige
        // `fmt::Error`). A perda de granularidade e' do **trait**, nao nossa:
        // quem precisa do erro tipado usa `escrever_rfc3339`.
        self.escrever_rfc3339(f).map_err(|_| fmt::Error)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;
    use crate::fixture::instante;

    /// Vetores de referencia de dias conhecidos (Julian Day Numbers).
    ///
    /// ## Por que estes e nao "qualquer data"
    ///
    /// Um teste de `de_dias_desde_epoch` que gerasse a entrada a partir da
    /// propria funcao seria tautologico -- passaria mesmo com a formula errada.
    /// Estes numeros vem da literature (algoritmo de Meeus, Astronomical
    /// Algorithms, capitulo 7) e sao independentes da implementacao.
    const VETORES: [(i64, i64, u8, u8); 6] = [
        (0, 1970, 1, 1),       // epoch Unix
        (11, 1970, 1, 12), // dia 11 do mes: 1970-01-12
        (-1, 1969, 12, 31),    // dia anterior ao epoch
        (59, 1970, 3, 1),      // 1970 nao e' bissexto
        (364, 1970, 12, 31),
        (10_957, 2000, 1, 1), // virada de milenio
    ];

    #[test]
    fn de_dias_desde_epoch_bate_com_vetores_conhecidos() {
        for (dias, ano, mes, dia) in VETORES {
            let obtido = DataHoraCivil::de_dias_desde_epoch(dias);
            exigir!(
                obtido == (ano, mes, dia),
                "dia {dias} deve ser {ano}-{mes}-{dia}",
                "{:?}",
                obtido
            );
        }
    }

    #[test]
    fn dias_desde_epoch_bate_com_vetores_conhecidos() {
        for (dias, ano, mes, dia) in VETORES {
            let obtido = dias_desde_epoch(ano, mes, dia);
            exigir!(
                obtido == dias,
                "{ano}-{mes}-{dia} deve ser dia {dias}",
                "{obtido}"
            );
        }
    }

    #[test]
    fn ida_e_volta_entre_dias() {
        // Invariante I2, em uma varredura que inclui viradas de ano, de
        // bissexto e de seculo.
        for dias in (0..40_000).step_by(37) {
            let (ano, mes, dia) = DataHoraCivil::de_dias_desde_epoch(dias);
            exigir!(
                dias_desde_epoch(ano, mes, dia) == dias,
                "ida e volta no dia {dias}",
                "-> {ano}-{mes}-{dia} -> {}",
                dias_desde_epoch(ano, mes, dia)
            );
        }
    }

    #[test]
    fn dia_da_semana_conhecido() {
        // 1970-01-01 foi quinta; 2000-01-01 foi sabado.
        exigir!(
            DataHoraCivil::dia_da_semana_de_dias(0) == 4,
            "epoch Unix = quinta",
            "{}",
            DataHoraCivil::dia_da_semana_de_dias(0)
        );
        exigir!(
            DataHoraCivil::dia_da_semana_de_dias(10_516) == 6,
            "2000-01-01 = sabado",
            "{}",
            DataHoraCivil::dia_da_semana_de_dias(10_516)
        );
    }

    #[test]
    fn imf_fixdate_e_em_ingles_e_em_gmt() {
        // 2026-10-09 e' uma sexta-feira.
        let i = instante(2026, 10, 9, 17, 3, 22, 0);
        let mut saida = String::new();
        let r = i.civil_utc().escrever_imf_fixdate(&mut saida);
        exigir!(r.is_ok(), "escrita do header", "{r:?}");
        exigir!(
            saida == "Fri, 09 Oct 2026 17:03:22 GMT",
            "IMF-fixdate conforme RFC 9110",
            "{saida}"
        );
    }

    #[test]
    fn extenso_pt_br_usa_nomes_em_portugues() {
        let i = instante(2026, 10, 9, 17, 3, 22, 0);
        let mut saida = String::new();
        let r = i.civil_utc().escrever_extenso_pt_br(&mut saida);
        exigir!(r.is_ok(), "escrita por extenso", "{r:?}");
        exigir!(
            saida == "sexta-feira, 09/10/2026 17:03:22 UTC",
            "dia da semana em pt-BR",
            "{saida}"
        );
        // E' o mesmo dado do header, em outro idioma: os doisvem da mesma
        // tabela de indices, entao nao podem divergir.
        let mut header = String::new();
        let _ = i.civil_utc().escrever_imf_fixdate(&mut header);
        exigir!(
            saida.starts_with("sexta-feira") && header.starts_with("Fri"),
            "mesma data, dois idiomas",
            "{saida} | {header}"
        );
    }

    #[test]
    fn data_extenso_por_extenso_em_portugues() {
        let mut saida = String::new();
        let r = instante(2026, 10, 9, 0, 0, 0, 0)
            .civil_utc()
            .escrever_data_extenso_pt_br(&mut saida);
        exigir!(r.is_ok(), "escrita da data", "{r:?}");
        exigir!(
            saida == "nove de outubro de dois mil e vinte e seis",
            "data por extenso (nota fiscal exige)",
            "{saida}"
        );
    }

    #[test]
    fn numeros_por_extenso_cobrem_todo_o_mes() {
        // Vetores exatos para 1..=31: um dia do mes e' o caso de uso direto,
        // e texto aproximado passaria por erro de concordancia.
        const ESPERADOS: [&str; 31] = [
            "um", "dois", "três", "quatro", "cinco", "seis", "sete", "oito", "nove", "dez",
            "onze", "doze", "treze", "catorze", "quinze", "dezesseis", "dezessete", "dezoito",
            "dezenove", "vinte", "vinte e um", "vinte e dois", "vinte e três", "vinte e quatro",
            "vinte e cinco", "vinte e seis", "vinte e sete", "vinte e oito", "vinte e nove",
            "trinta", "trinta e um",
        ];
        for (i, esperado) in ESPERADOS.iter().enumerate() {
            let dia = u32::try_from(i + 1).unwrap_or_default();
            let obtido = numero_por_extenso_pt_br(dia);
            exigir!(
                obtido == *esperado,
                "dia {dia} por extenso",
                "esperava {esperado:?}, veio {obtido:?}"
            );
        }
    }

    #[test]
    fn centenas_tem_forma_propria_e_um_e_de_ligacao() {
        // O bug classico do extenso: "cento e um" (com "e") e "cento e um
        // mil"? Nao. O que importa e: 101 = "cento e um" -- o "e" entra uma
        // vez, nao duas.
        const CASOS: [(u32, &str); 8] = [
            (100, "cem"),
            (101, "cento e um"),
            (110, "cento e dez"),
            (121, "cento e vinte e um"),
            (200, "duzentos"),
            (900, "novecentos"),
            (926, "novecentos e vinte e seis"),
            (996, "novecentos e noventa e seis"),
        ];
        for (numero, esperado) in CASOS {
            let obtido = numero_por_extenso_pt_br(numero);
            exigir!(
                obtido == esperado,
                "{numero} por extenso",
                "esperava {esperado:?}, veio {obtido:?}"
            );
        }
    }

    #[test]
    fn zero_e_dezenove_tem_forma_propria() {
        exigir!(numero_por_extenso_pt_br(0) == "zero", "zero", "{}", numero_por_extenso_pt_br(0));
        // "dezenove" e' uma palavra so: cair em "dez e nove" seria errado.
        exigir!(
            numero_por_extenso_pt_br(19) == "dezenove",
            "dezenove",
            "{}",
            numero_por_extenso_pt_br(19)
        );
    }

    #[test]
    fn deslocamento_zero_e_z() {
        let mut saida = String::new();
        let r = instante(2026, 1, 1, 0, 0, 0, 0)
            .civil_utc()
            .escrever_deslocamento(&mut saida);
        exigir!(r.is_ok(), "escrita", "{r:?}");
        exigir!(saida == "Z", "UTC renderiza como Z", "{saida}");
    }

    #[test]
    fn deslocamento_negativo_e_menos_tres_horas() {
        let mut saida = String::new();
        let r = instante(2026, 1, 1, 0, 0, 0, 0)
            .civil_com_deslocamento(-10_800)
            .escrever_deslocamento(&mut saida);
        exigir!(r.is_ok(), "escrita", "{r:?}");
        exigir!(saida == "-03:00", "Brasilia = -03:00", "{saida}");
    }
}
