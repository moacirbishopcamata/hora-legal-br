//! Injeta a identidade do build no binario, via variaveis de ambiente
//! de compilacao.
//!
//! ## Por que um `build.rs`
//!
//! `env!("CARGO_PKG_VERSION")` so muda quando alguem edita o `Cargo.toml`. O
//! requisito e' que **cada commit** produza uma versao distinta e rastreavel.
//! Isso exige dados do git, que so o `build.rs` consegue ler -- `env!` roda
//! em tempo de compilacao do crate final e nao tem acesso a subprocesso.
//!
//! ## Resiliencia do proprio build
//!
//! Um `build.rs` que falha derruba o build inteiro. Em container CI sem
//! `.git` (checkout raso, `docker build` sem historico), `git` nao existe -- e
//! um build quebrado por falta de git e' pior do que uma versao sem hash.
//! Por isso: **nenhum comando falha o build**; na pior das hipoteses, os
//! campos ficam com o marcador [`INDISPONIVEL`].

use std::process::Command;

/// Marcador usado quando o git nao esta disponivel ou o build rodou sem
/// historico.
///
/// Distinto de um hash real: o log de acesso nunca deve confundir "nao sei"
/// com "commit `0000000`".
const INDISPONIVEL: &str = "desconhecido";

/// Executa `comando` e devolve a saida aparada, ou [`INDISPONIVEL`].
///
/// Recebe `&mut Command` porque `Command::args` devolve `&mut Command`: quem
/// chama monta a cadeia e passa o resultado, em vez de devolver o proprio
/// `Command` para que o chamador descubra que precisa de `mut`.
///
/// ## Pre-condicao
///
/// Nenhuma. Um comando inexistente, um repositorio sem `HEAD` e uma saida
/// vazia sao todos "nao sei" -- e `INDISPONIVEL` e' a resposta correta para
/// os tres.
///
/// ## Pos-condicao
///
/// O resultado nunca contem `\n`: e' a saida **aparada**. Um `\n` no meio de
/// um valor de `cargo:rustc-env` corromperia o arquivo de ambiente que o
/// `rustc` gera, e o sintome apareceria como "string invalida" em um arquivo
/// que ninguem suspeitaria.
fn saida_de(comando: &mut Command) -> String {
    match comando.output() {
        Ok(saida) if saida.status.success() => {
            let texto = String::from_utf8_lossy(&saida.stdout).trim().to_string();
            if texto.is_empty() {
                INDISPONIVEL.to_owned()
            } else {
                texto
            }
        }
        // Processo inexistente, codigo diferente de zero, ou `io::Error`:
        // todos viram "nao sei", sem ruido no log do build.
        _ => INDISPONIVEL.to_owned(),
    }
}

/// Atalho: monta o `git` com `argumentos`, executa e apara.
///
/// **Total**: nunca falha -- ver [`saida_de`].
fn git(argumentos: &[&str]) -> String {
    saida_de(Command::new("git").args(argumentos))
}

/// Hash completo de `HEAD`, ou `None` se nao houver repositorio.
///
/// `Option` e nao `INDISPONIVEL` porque o chamador usa a ausencia para
/// decidir se declara `rerun-if-changed`; um `Some("desconhecido")` levaria
/// a um caminho de reexecucao sem fim.
fn principal() -> Option<String> {
    let saida = git(&["rev-parse", "HEAD"]);
    (saida != INDISPONIVEL).then_some(saida)
}

/// Hash abreviado de `HEAD` (7 caracteres por padrao do git).
fn principal_curto() -> String {
    git(&["rev-parse", "--short", "HEAD"])
}

/// Numero de commits ate `HEAD`.
///
/// E' este valor que cumpre o requisito "incrementar a versao a cada commit":
/// ele e' estritamente monotonico **por construcao do git**, sem depender de
/// ninguem lembrar de editar um numero em um arquivo.
fn numero_do_build() -> String {
    git(&["rev-list", "--count", "HEAD"])
}

/// Data do commit em ISO 8601.
///
/// `-c core.quotepath=false` evita que nomes de arquivo nao-ASCII (que este
/// repositorio tem) venham escapados no log.
fn data_do_commit() -> String {
    git(&[
        "-c",
        "core.quotepath=false",
        "log",
        "-1",
        "--pretty=format:%cI",
    ])
}

/// Ramo atual. [`INDISPONIVEL`] em CI, onde o `HEAD` e' destacado por padrao.
fn ramo() -> String {
    git(&["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Publica uma variavel de compilacao, com o fallback aplicado uma unica vez.
///
/// Existe para que a politica "nenhum campo fica vazio" seja **estrutural**:
/// um `unwrap_or_else` aqui e nao em seis lugares, entao um campo novo nunca
/// pode esquecer o fallback.
fn publica(nome: &str, valor: Option<String>) {
    let valor = valor.unwrap_or_else(|| INDISPONIVEL.to_owned());
    println!("cargo:rustc-env={nome}={valor}");
}

fn main() {
    // Reexecuta se o proprio script mudar, ou se o `HEAD` mudar: `cargo build`
    // seguinte reflete o novo commit sem `clean`.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=VERSAO_GIT");

    match principal() {
        Some(hash) => println!("cargo:rerun-if-changed={hash}"),
        // Sem repositorio, a unica coisa que muda o resultado e' o ambiente.
        None => println!("cargo:rerun-if-env-changed=GIT_DIR"),
    }

    publica("HORA_LEGAL_PRINCIPAL", principal());
    publica("HORA_LEGAL_PRINCIPAL_CURTO", Some(principal_curto()));
    publica("HORA_LEGAL_NUMERO_DO_BUILD", Some(numero_do_build()));
    publica("HORA_LEGAL_DATA_DO_COMMIT", Some(data_do_commit()));
    publica("HORA_LEGAL_RAMO", Some(ramo()));
    publica(
        "HORA_LEGAL_VERSAO_SEMANTICA",
        Some(env!("CARGO_PKG_VERSION").to_owned()),
    );
}