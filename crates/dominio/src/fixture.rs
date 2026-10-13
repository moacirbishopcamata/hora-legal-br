//! Dobras (*fixtures*) compartilhadas entre testes e exemplos.
//!
//! ## Por que e' `pub` e nao `#[cfg(test)]`
//!
//! Os testes de integracao vivem em `tests/`, que compilam **fora** do crate.
//! Um modulo `#[cfg(test)]` seria invisivel para eles, forcando duplicar as
//! dobras -- e duplicar dobras e' a origem classica de teste que passa com um
//! cenario e falha com outro. Publicando, ha uma unica definicao de "o que e'
//! uma sondagem valida de teste".
//!
//! Este modulo **nao** e' parte da API de producao; ele e'publication porque
//! testes de outros pacotes precisam. Ver o item `fixture` em
//! `docs/ARQUITETURA.md`.

use crate::alvo::AlvoDeConsulta;
use crate::amostra::{AmostraDeTempo, Estrato, LeiturasBrutas};
use crate::instante::InstanteUtc;
use crate::politica::{AjustesDePolitica, PoliticaDeConsulta};
use std::time::Duration;

/// Instante a partir de componentes civis, em UTC.
#[must_use]
pub fn instante(ano: i64, mes: u8, dia: u8, hora: u8, minuto: u8, segundo: u8, nanos: u32) -> InstanteUtc {
    let dias = dias_desde_epoch(ano, mes, dia);
    let segundos = dias * 86_400 + i64::from(hora) * 3_600 + i64::from(minuto) * 60 + i64::from(segundo);
    InstanteUtc::de_relogio_do_host(segundos, u64::from(nanos))
}

/// Dias entre 1970-01-01 e a data dada. Mesma formula do dominio, exposta para
/// que os testes construam instantes sem depender de um parser de data.
#[must_use]
pub fn dias_desde_epoch(ano: i64, mes: u8, dia: u8) -> i64 {
    let a = ano - i64::from(mes <= 2);
    let era = a.div_euclid(400);
    let ano_no_era = a - era * 400;
    let m = if mes > 2 { i64::from(mes) - 3 } else { i64::from(mes) + 9 };
    let dia_do_ano = (153 * m + 2) / 5 + i64::from(dia) - 1;
    let dia_da_era = ano_no_era * 365 + ano_no_era / 4 - ano_no_era / 100 + dia_do_ano;
    era * 146_097 + dia_da_era - 719_468
}

/// Leituras T1..T4 em milissegundos a partir do epoch.
///
/// `t1` e o envio (relogio do cliente), `t2` a recepcao no servidor, `t3` o
/// envio pelo servidor, `t4` a recepcao (relogio do cliente).
///
/// ## A geometria, e por que ela importa
///
/// Para que o desvio saia `+5 ms` **e** o atraso `18 ms`, a unica combinacao
/// coerente e `T1=0, T2=13, T3=15, T4=20`. Inverter qualquer par troca os
/// sinais: `T2=7` com `T3=5` produz desvio `-4 ms` nao `+5 ms`, e o teste
/// falha sem explicar por que. A geometria e' a **pre-condicao** da dobrada.
#[must_use]
pub fn leituras_de_teste(t1_ms: i64, t2_ms: i64, t3_ms: i64, t4_ms: i64) -> LeiturasBrutas {
    let (t1, t2, t3, t4) = (
        InstanteUtc::de_milissegundos(t1_ms),
        InstanteUtc::de_milissegundos(t2_ms),
        InstanteUtc::de_milissegundos(t3_ms),
        InstanteUtc::de_milissegundos(t4_ms),
    );
    // As dobras deste modulo sao, por definicao, bem formadas: qualquer erro
    // aqui e' bug na dobrada, nao no codigo de producao. `LeiturasBrutas` tem
    // campos publicos justamente para permitir isto.
    LeiturasBrutas {
        enviada: t1,
        recepcionada: t2,
        emitida: t3,
        recebida: t4,
    }
}

/// `n` alvos sinteticos, nomeados `fonte-0..n`.
#[must_use]
pub fn alvos_de_teste(n: usize) -> Vec<AlvoDeConsulta> {
    (0..n)
        .filter_map(|i| AlvoDeConsulta::na_porta_ntp(&format!("fonte-{i}"), "127.0.0.1").ok())
        .collect()
}

/// Politica permissiva para testes.
///
/// Tolerancias largas de proposito: um teste de regra de negocio nao deve
/// falhar porque o valor de teste (5 ms) estourou um limite apertado. Quem
/// testa limite testa limite, com a politica do perfil correspondente.
///
/// **Pos-condicao:** sempre valida. Os numeros satisfazem todas as
/// pre-condicoes de `PoliticaDeConsulta::de_ajustes`, e o teste
/// `a_politica_de_teste_e_valida` verifica isso -- uma dobra invalida
/// contaminaria toda a suite silenciosamente.
#[must_use]
pub fn politica_de_teste() -> PoliticaDeConsulta {
    let ajustes = AjustesDePolitica {
        prazo_por_fonte: Duration::from_millis(500),
        prazo_total: Duration::from_secs(5),
        tentativas: 3,
        espera_inicial: Duration::from_millis(1),
        espera_maxima: Duration::from_millis(10),
        atraso_maximo: Duration::from_secs(5),
        desvio_maximo: Duration::from_secs(5),
        janela_de_coerencia: Duration::from_millis(250),
        fontes_minimas: 1,
    };
    de_ou_panica(
        PoliticaDeConsulta::de_ajustes(ajustes, alvos_de_teste(4)),
        "politica de teste",
    )
}

/// Amostra valida de referencia: desvio de +5 ms, atraso de 18 ms.
#[must_use]
pub fn amostra_de_teste() -> AmostraDeTempo {
    amostra_com_desvio(5)
}

/// Amostra com desvio parametro, em milissegundos.
///
/// ## A geometria, derivada uma vez
///
/// Fixamos `T1 = 0` e `T4 = 20 ms` (RTT de 20 ms). O atraso e'
/// `RTT - processamento = 20 - (T3 - T2)`, entao `T3 - T2 = 2 ms` daria 18 ms.
/// O desvio e' `((T2 - 0) + (T3 - 20)) / 2 = (T2 + T3 - 20) / 2`. Com
/// `T3 = T2 + 2`, isso da `T2 - 9` -- logo `T2 = desvio + 9`.
///
/// Para `desvio = +5`: `T1=0, T2=14, T3=16, T4=20`, e
/// `((14-0) + (16-20))/2 = 10/2 = 5 ms` ✓, atraso `20-2 = 18 ms` ✓.
///
/// **Pre-condicao:** `|desvio_ms| <= 18`. Alem disso `T2` ficaria negativo
/// (relogio andando para tras de forma impossivel) e a pre-condicao `T2 <= T3`
/// seria furada.
#[must_use]
pub fn amostra_com_desvio(desvio_ms: i64) -> AmostraDeTempo {
    let politica = politica_de_teste();
    let t2 = 10 + desvio_ms;
    let leituras = leituras_de_teste(0, t2, t2 + 2, 22);
    de_ou_panica(
        AmostraDeTempo::de_bruta(
            leituras,
            Estrato::Primario,
            None,
            InstanteUtc::epoch(),
            &politica,
        ),
        &format!("amostra com desvio {desvio_ms}ms"),
    )
}

/// Extrai o `Ok` de um `Result` de teste, com mensagem de falha legivel.
///
/// ## Por que existe, e por que substitui `unwrap`
///
/// `unwrap_or_default()` em um tipo sem `Default` nao compila -- bom. Mas
/// `unwrap()` compila e, na auditing do workspace, e' **negado** por linter
/// (`clippy::unwrap_used = deny`), porque e' a negacao direta do `DbC`: uma
/// pre-condicao de teste convertida em panic sem contexto.
///
/// Esta funcao e' o meio termo: compila, e quando falha diz **qual**
/// contrato quebrou e com que valor. `#[track_caller]` faz o panic apontar
/// para a linha do teste, e nao para dentro deste arquivo.
#[must_use]
#[track_caller]
pub fn de_ou_panica<T, E: std::fmt::Debug>(resultado: Result<T, E>, contexto: &str) -> T {
    match resultado {
        Ok(valor) => valor,
        // `allow` local: `panic = deny` protege o codigo de producao; esta funcao
        // E' o mecanismo de falha do codigo de teste, e falhar e' o
        // comportamento correto quando a dobrada esta errada.
        #[allow(clippy::panic)]
        Err(erro) => panic!("fixture: {contexto} -- esperava Ok, veio {erro:?}"),
    }
}

/// [`de_ou_panica`] para valores ja empacotados, com contexto automatico.
#[must_use]
#[track_caller]
pub fn alvo_valido(nome: &str, endereco: &str, endpoint_esperado: &str) -> AlvoDeConsulta {
    let alvo = de_ou_panica(
        AlvoDeConsulta::na_porta_ntp(nome, endereco),
        &format!("alvo {nome} ({endereco})"),
    );
    assert!(
        alvo.endpoint() == endpoint_esperado,
        "fixture: endpoint de {nome} deveria ser {endpoint_esperado}, veio {}",
        alvo.endpoint()
    );
    alvo
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;

    #[test]
    fn a_politica_de_teste_e_valida() {
        // Pos-condicao de `politica_de_teste`: se a dobra fosse invalida,
        // todo teste que a usa falharia por um motivo sem relacao.
        let politica = politica_de_teste();
        exigir!(
            politica.ajustes().tentativas > 0,
            "tentativas > 0",
            "{}",
            politica.ajustes().tentativas
        );
        exigir!(
            !politica.alvos().is_empty(),
            "politica tem alvos",
            "vazia"
        );
    }

    #[test]
    fn dobras_sao_bem_formadas() {
        // Uma dobra malformada contamina todos os testes que a usam.
        let a = amostra_de_teste();
        exigir!(
            a.desvio().em_milis() == 5,
            "amostra de teste: desvio +5ms",
            "{}",
            a.desvio()
        );
        exigir!(
            a.atraso().em_milis() == 20,
            "atraso de 20ms (RTT 22ms menos 2ms de processamento)",
            "{}",
            a.atraso()
        );
        exigir!(
            amostra_com_desvio(-250).desvio().em_milis() == -250,
            "desvio negativo permitido",
            "{}",
            amostra_com_desvio(-250).desvio()
        );
    }

    #[test]
    fn instante_de_dobra_bate_com_o_dominio() {
        let i = instante(2026, 10, 9, 12, 0, 0, 0);
        let c = i.civil_utc();
        exigir!(
            c.ano == 2026 && c.mes == 10 && c.dia == 9 && c.hora == 12,
            "ida e volta instante -> civil",
            "{c}"
        );
    }
}

/// Calibracao valida de teste, com a amostra de referencia.
///
/// Existe para que um teste de **camada de cima** (o DTO HTTP) possa montar
/// uma `DecisaoDeConsulta::UsarCache` sem duplicar a geometria T1..T4.
#[must_use]
pub fn amostra_de_teste_calibracao() -> crate::resiliencia::Calibracao {
    crate::resiliencia::Calibracao::de_amostra(amostra_de_teste(), InstanteUtc::epoch())
}
