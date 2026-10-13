//! Fusos e a Hora Legal Brasileira.
//!
//! ## Por que o deslocamento e' fixo
//!
//! O Brasil nao observa horario de verao desde o Decreto 9.772, de 26 de
//! abril de 2019. Portanto a Hora Legal Brasileira e' **UTC-03:00 o ano
//! inteiro**, sem excecao -- e um `Offset` fixo e' mais honesto (e mais
//! barato) do que uma tabela de transicao que nunca muda.
//!
//! O proprio NTP.br registra o mesmo: o protocolo trabalha so em UTC e nao
//! interfere em fuso nem em horario de verao.
//!
//! Isso **nao** significa que o fuso do host importe: ver
//! [`crate::host`] para a decisao de escopo.

use crate::contrato::ErroDeContrato;
use crate::exigir;
use crate::instante::InstanteUtc;

/// Deslocamento fixo de um fuso, em segundos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeslocamentoDeFuso(i32);

impl DeslocamentoDeFuso {
    /// UTC. Renderizado como `Z` em RFC 3339.
    pub const UTC: Self = Self(0);
    /// Hora Legal Brasileira: UTC-03:00, sem horario de verao desde 2019.
    pub const BRASILIA: Self = Self(-3 * 3_600);

    /// Constroi um deslocamento a partir de horas inteiras.
    ///
    /// Pre-condicao: `-23 <= horas <= 23`.
    pub fn de_horas(horas: i32) -> Result<Self, ErroDeContrato> {
        // `(-23..=23).contains` nao e' `const fn` ate o stable atual; o
        // intervalo e' minusculo e o desvio e' zero, entao o custo e' nulo.
        if !(u32::try_from(horas + 23).is_ok_and(|h| h <= 46)) {
            return Err(ErroDeContrato::novo(
                "DeslocamentoDeFuso::de_horas",
                format!("horas={horas} fora de -23..=23"),
            ));
        }
        Ok(Self(horas * 3_600))
    }

    /// Deslocamento em segundos.
    #[must_use]
    pub const fn em_segundos(self) -> i32 {
        self.0
    }

    /// Deslocamento em horas inteiras (divisao exata por construcao).
    #[must_use]
    pub const fn em_horas(self) -> i32 {
        self.0 / 3_600
    }

    /// Deslocamento em horas com fracao -- o valor que o `chrono` chama de
    /// `-03:00` e que o JavaScript chama de `-180` (minutos).
    #[must_use]
    pub const fn em_minutos(self) -> i32 {
        self.0 / 60
    }
}

impl std::fmt::Display for DeslocamentoDeFuso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 == 0 {
            return f.write_str("Z");
        }
        let sinal = if self.0 < 0 { '-' } else { '+' };
        let absoluto = self.0.unsigned_abs();
        write!(f, "{sinal}{:02}:{:02}", absoluto / 3_600, (absoluto % 3_600) / 60)
    }
}

/// A Hora Legal Brasileira como objeto: UTC-03:00, sempre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HoraLegalBrasileira;

impl HoraLegalBrasileira {
    /// Deslocamento aplicavel a qualquer instante da historia moderna.
    pub const DESLOCAMENTO: DeslocamentoDeFuso = DeslocamentoDeFuso::BRASILIA;

    /// Instante -> hora civil de Brasilia.
    ///
    /// Pos-condicao: o calendario civil resultante tem deslocamento -03:00.
    #[must_use]
    pub fn civil_de(instante: InstanteUtc) -> crate::civil::DataHoraCivil {
        let civil = instante.civil_com_deslocamento(Self::DESLOCAMENTO.em_segundos());
        exigir!(
            civil.deslocamento_segundos == -10_800,
            "HoraLegalBrasileira::civil_de",
            "deslocamento deveria ser -03:00, veio {}",
            civil.deslocamento_segundos
        );
        civil
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::fixture::instante;

    #[test]
    fn brasilia_e_utc_menos_tres() {
        exigir!(
            HoraLegalBrasileira::DESLOCAMENTO.em_horas() == -3,
            "Brasilia: -03:00",
            "{}",
            HoraLegalBrasileira::DESLOCAMENTO.em_horas()
        );
        exigir!(
            HoraLegalBrasileira::DESLOCAMENTO.em_segundos() == -10_800,
            "Brasilia: -10800s",
            "{}",
            HoraLegalBrasileira::DESLOCAMENTO.em_segundos()
        );
    }

    #[test]
    fn civil_de_brasilia_e_tres_horas_atras_do_utc() {
        let utc = instante(2026, 1, 15, 12, 0, 0, 0);
        let civil = HoraLegalBrasileira::civil_de(utc);
        exigir!(civil.hora == 9, "Brasilia: 12Z sao 09h locais", "{}", civil.hora);
        exigir!(civil.mes == 1 && civil.dia == 15, "data preservada", "{civil}");
    }

    #[test]
    fn virada_de_meia_noite_local_nao_embaralha() {
        // 02:00Z de 16/01 e' 23:00 de 15/01 em Brasilia: o dia local e'
        // anterior ao UTC. E' o caso que quebra implementacoes ingenuas.
        let utc = instante(2026, 1, 16, 2, 0, 0, 0);
        let civil = HoraLegalBrasileira::civil_de(utc);
        exigir!(
            civil.dia == 15 && civil.hora == 23,
            "Brasilia: 02Z de 16/01 = 23h de 15/01",
            "{civil}"
        );
    }

    #[test]
    fn utc_renderiza_como_z() {
        exigir!(DeslocamentoDeFuso::UTC.to_string() == "Z", "UTC como Z", "{}", DeslocamentoDeFuso::UTC);
    }

    #[test]
    fn deslocamento_invalido_e_rejeitado_na_pre_condicao() {
        let r = DeslocamentoDeFuso::de_horas(24);
        exigir!(r.is_err(), "pre-condicao: 24h nao e' fuso", "{r:?}");
    }
}