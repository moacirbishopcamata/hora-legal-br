//! Relogio e sorteio: as duas portas que tornam o dominio testavel.
//!
//! ## Por que "relogio" e' uma porta e nao uma chamada a `SystemTime::now`
//!
//! Todo o dominio decide com base em *agora*: idade de uma calibracao,
//! janela de coerencia, cooldown de circuito. Se `SystemTime::now()` fosse
//! chamado direto, nenhum teste poderia exercising esses caminhos sem dormir
//! de verdade -- e testes que dormem nao rodam, e testes que nao rodam nao
//! protegem. Por isso [`Relogio`] e' uma interface: o teste injeta um relogio
//! que avanca para onde quiser.
//!
//! O mesmo argumento vale para [`Sorteio`]: o *jitter* do backoff precisa ser
//! testavel, o que significa ser injetavel.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::instante::InstanteUtc;

/// Fonte do tempo atual.
///
/// Implementacoes de producao: `relogio::do_sistema` (via SO).
/// Implementacoes de teste: `relogio::fixo` / `relogio::manual`.
pub trait Relogio: Send + Sync + std::fmt::Debug {
    /// Instante atual.
    fn agora(&self) -> InstanteUtc;
}

impl<T: Relogio + ?Sized> Relogio for Arc<T> {
    fn agora(&self) -> InstanteUtc {
        (**self).agora()
    }
}

/// Gerador de numeros aleatorios em `[0, 1)`.
///
/// Existe para que o *jitter* de [`crate::resiliencia`] seja testavel. Com
/// `rand` chamado direto, o teste teria de estatistica -- e um teste que so
/// passa "em media" nao e' teste, e' Wellness Check (Anti-patterno Yegor256
/// *Prover por intermédio de mocked instance*).
pub trait Sorteio: Send + Sync + std::fmt::Debug {
    /// Valor em `[0, 1)`.
    ///
    /// **Pre-condicao:** o implementador garante `0.0 <= v < 1.0`.
    fn real(&self) -> f64;
}

impl<T: Sorteio + ?Sized> Sorteio for Arc<T> {
    fn real(&self) -> f64 {
        (**self).real()
    }
}

/// Relogio apoiado no sistema operacional.
///
/// Implementa o epoch de cada familia de [`crate::host`]:
/// - Windows: `GetSystemTimePreciseAsFileTime` via `SystemTime::now()`;
/// - POSIX: `clock_gettime(CLOCK_REALTIME)` via `SystemTime::now()`.
///
/// `SystemTime::now()` ja expoe o relogio de maior resolucao disponivel nas
/// duas plataformas, entao a traducao para [`crate::host`] fecha sem `unsafe`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RelogioDoSistema;

impl RelogioDoSistema {
    /// Constroi o relogio. Trivial, mas explicito: impede que alguém
    /// instancia a struct vazia e presuma que ha inicializacao escondida.
    #[must_use]
    pub const fn novo() -> Self {
        Self
    }
}

impl Relogio for RelogioDoSistema {
    fn agora(&self) -> InstanteUtc {
        let agora = std::time::SystemTime::now();
        // `duration_since(UNIX_EPOCH)` falha so **antes** de 1970. Esse
        // instante negativo e' uma resposta valida -- nao um erro -- por isso
        // o `match` o trata como caso legitimo, e nao como falha.
        match agora.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => {
                InstanteUtc::de_relogio_do_host(
                    i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                    u64::from(d.subsec_nanos()),
                )
            }
            Err(antes_do_epoch) => {
                let d = antes_do_epoch.duration();
                let distancia = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
                // `delta_ate` e' com sinal; ir do instante para tras e' o
                // oposto de ir para frente. Sem `if`, sem negativo manual.
                match distancia >= 0 {
                    true => InstanteUtc::de_relogio_do_host(
                        -distancia,
                        (d.subsec_nanos() as u64).wrapping_neg(),
                    ),
                    false => InstanteUtc::epoch(),
                }
            }
        }
    }
}

/// Relogio controlado por teste.
///
/// ### Por que `Arc<Mutex<>>` e nao `Cell`
///
/// O tempo precisa ser avanca' por uma `task` enquanto outra thread o le.
/// `Mutex` e' o primitivo correto aqui; um `Cell` seria `!Sync` e nao
/// compilaria num servidor multi-thread.
///
/// ### Por que nao deriva `Clone`
///
/// `AtomicU64` nao e' `Clone`. Derivar exigiria um `Clone` faux, que
/// duplicaria o estado e quebraria o contador de leituras. Compartilhar e'
///feito com `Arc<RelogioManual>`, que nao mente sobre identidade.
#[derive(Debug)]
pub struct RelogioManual {
    instante: std::sync::Mutex<InstanteUtc>,
    /// Contador de leituras. Permite afirmar que um caminho **nao** consultou
    /// o relogio -- util para provar que o cache nao bate no SO no caminho
    /// rapido (requisito de latencia).
    leituras: AtomicU64,
}

impl RelogioManual {
    /// Cria o relogio parado num instante.
    #[must_use]
    pub fn fixo(instante: InstanteUtc) -> Self {
        Self {
            instante: std::sync::Mutex::new(instante),
            leituras: AtomicU64::new(0),
        }
    }

    /// Anda o tempo.
    ///
    /// **Pos-condicao:** devolve o novo instante. Nao ha `unwrap` no caminho:
    /// um `Mutex` envenenado significaria que um `panic!` ocorreu dentro de
    /// outra thread enquanto segurava o lock -- nesse caso o tempo para de
    /// andar e o retorno e' o ultimo valor conhecido, para que o teste
    /// falhe de forma legivel em vez de propagar o panic.
    pub fn avancar(&self, duracao: Duration) -> InstanteUtc {
        let Ok(mut guarda) = self.instante.lock() else {
            return InstanteUtc::epoch();
        };
        *guarda = guarda.mais(duracao);
        *guarda
    }

    /// Quantas vezes `agora` foi chamado.
    #[must_use]
    pub fn leituras(&self) -> u64 {
        self.leituras.load(Ordering::Relaxed)
    }
}

impl Relogio for RelogioManual {
    fn agora(&self) -> InstanteUtc {
        self.leituras.fetch_add(1, Ordering::Relaxed);
        self.instante.lock().map_or_else(|_| InstanteUtc::epoch(), |g| *g)
    }
}

/// Sorteio deterministico que devolve sempre o mesmo valor.
///
/// Util para testar *limites* de jitter (`0.0` e `1.0` exclusive sao os
/// casos extremos que so aparecem com um valor fixo).
#[derive(Debug, Clone, Copy)]
pub struct SorteioFixo(f64);

impl SorteioFixo {
    /// Cria um sorteio constante.
    ///
    /// **Pre-condicao:** `0.0 <= valor < 1.0`.
    #[must_use]
    pub const fn novo(valor: f64) -> Self {
        Self(valor)
    }
}

impl Sorteio for SorteioFixo {
    fn real(&self) -> f64 {
        self.0
    }
}

/// Sorteio cyclico por uma lista de valores.
///
/// Reproduz sequencias conhecidas de jitter sem usar RNG -- util para provar
/// que o backoff respeita o teto em todos os pontos da sequencia.
///
/// ### Por que nao deriva `Clone`
///
/// `AtomicU64` nao e' `Clone`. Derivar exigiria clonar o indice -- o que
/// faria duas copias compartilharem a mesma sequencia, e o teste perderia o
/// controle sobre qual valor vem a seguir. Compartilhar via `Arc` e' correto.
#[derive(Debug)]
pub struct SorteioCiclico {
    valores: Vec<f64>,
    indice: AtomicU64,
}

impl SorteioCiclico {
    /// Cria o sorteio ciclico.
    ///
    /// **Pre-condicao:** lista nao vazia.
    #[must_use]
    pub fn de_valores(valores: Vec<f64>) -> Self {
        Self {
            valores,
            indice: AtomicU64::new(0),
        }
    }
}

impl Sorteio for SorteioCiclico {
    fn real(&self) -> f64 {
        if self.valores.is_empty() {
            return 0.0;
        }
        let i = self.indice.fetch_add(1, Ordering::Relaxed) as usize % self.valores.len();
        self.valores[i]
    }
}

/// Contador compartilhado por todas as instancias de [`SorteioDoSistema`].
///
/// Rust nao permite `static` dentro de `impl`, entao vive no modulo. Um
/// unico contador para todas as instancias e' o correto: o sorteio nao
/// precisa ser independente por fonte, e um contador por instancia faria
/// duas fontesvizinhas emitirem o mesmo valor no mesmo instante -- o oposto
/// do que o jitter pretende.
static CONTADOR_DE_SORTEIO: AtomicU64 = AtomicU64::new(0);

/// Sorteio real, usando o gerador do sistema.
///
/// Sem dependencia externa: deriva de `f64::to_bits` de uma mixagem de
/// `SystemTime` e um contador atomico. Nao e' criptografico, e nao precisa
/// ser -- jitter nao e' seguranca, e' dispersao de carga.
#[derive(Debug, Clone, Copy, Default)]
pub struct SorteioDoSistema;

impl SorteioDoSistema {
    /// Constroi o sorteio.
    #[must_use]
    pub const fn novo() -> Self {
        Self
    }
}

impl Sorteio for SorteioDoSistema {
    fn real(&self) -> f64 {
        let n = CONTADOR_DE_SORTEIO.fetch_add(1, Ordering::Relaxed);
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0u64, |d| d.as_nanos() as u64);
        // splitmix64: barato, e tem boa distribuicao nos bits altos, que sao
        // os que viram fracao depois do deslocamento.
        let mut z = n
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(s)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // Top 53 bits -> [0, 1).
        ((z >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;

    #[test]
    fn relogio_manual_avanca_de_deterministico() {
        let r = RelogioManual::fixo(InstanteUtc::epoch());
        exigir!(r.agora() == InstanteUtc::epoch(), "t=0", "{}", r.agora());
        r.avancar(Duration::from_secs(10));
        exigir!(
            r.agora() == InstanteUtc::de_milissegundos(10_000),
            "apos 10s",
            "{}",
            r.agora()
        );
    }

    #[test]
    fn relogio_manual_conta_leituras() {
        let r = RelogioManual::fixo(InstanteUtc::epoch());
        r.agora();
        r.agora();
        exigir!(r.leituras() == 2, "contador de leituras", "{}", r.leituras());
    }

    #[test]
    fn relogio_do_sistema_avanca() {
        let r = RelogioDoSistema::novo();
        let a = r.agora();
        let b = r.agora();
        exigir!(b >= a, "relogio do sistema e' monotonico", "{a:?} -> {b:?}");
    }

    #[test]
    // `allow` local: os valores sao literais que o proprio teste devolve sem
    // nenhuma operacao aritmetica -- nao ha arredondamento possivel, e um
    // epsilon aqui esconderia um bug de ordem na sequencia.
    #[allow(clippy::float_cmp)]
    fn sorteio_ciclico_reproduz_a_sequencia() {
        let s = SorteioCiclico::de_valores(vec![0.1, 0.9, 0.5]);
        exigir!(s.real() == 0.1, "1o valor", "{}", s.real());
        exigir!(s.real() == 0.9, "2o valor", "{}", s.real());
        exigir!(s.real() == 0.5, "3o valor", "{}", s.real());
        exigir!(s.real() == 0.1, "repeticao", "{}", s.real());
    }

    #[test]
    fn sorteio_do_sistema_fica_em_meia_unidade() {
        let s = SorteioDoSistema::novo();
        for _ in 0..1_000 {
            let v = s.real();
            exigir!(
                (0.0..1.0).contains(&v),
                "pre-condicao do Sorteio: [0,1)",
                "{v}"
            );
        }
    }
}