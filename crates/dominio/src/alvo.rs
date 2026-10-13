//! Onde consultar: o alvo de uma sondagem NTP.
//!
//! `AlvoDeConsulta` e' um objeto, nao um `String`, porque ele carrega a
//! pre-condicao de estar no formato `host:porta` e porque a fonte precisa ser
//! *nomeada* -- a resposta da API diz de qual servidor veio a hora. Um `&str`
//! perderia as duas coisas.

use std::fmt;

use crate::contrato::ErroDeContrato;

/// Porta IANA do NTP. O padrao historico e' 123/UDP.
pub const PORTA_NTP: u16 = 123;

/// Servidor consultavel, validado na construcao.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AlvoDeConsulta {
    nome: String,
    endereco: String,
    porta: u16,
}

impl AlvoDeConsulta {
    /// Pre-condicoes:
    /// - `nome` nao vazio nem com espaco nas pontas;
    /// - `endereco` com 1..=253 caracteres;
    /// - `porta` nao zero.
    pub fn novo(nome: &str, endereco: &str, porta: u16) -> Result<Self, ErroDeContrato> {
        if nome.trim().is_empty() || nome != nome.trim() {
            return Err(ErroDeContrato::novo(
                "AlvoDeConsulta::novo",
                format!("nome invalido: {nome:?}"),
            ));
        }
        if endereco.is_empty() || endereco.len() > 253 {
            return Err(ErroDeContrato::novo(
                "AlvoDeConsulta::novo",
                format!("endereco invalida: {endereco:?}"),
            ));
        }
        if porta == 0 {
            return Err(ErroDeContrato::novo(
                "AlvoDeConsulta::novo",
                "porta 0 e' reservada e nao roteavel",
            ));
        }
        Ok(Self {
            nome: nome.to_owned(),
            endereco: endereco.to_owned(),
            porta,
        })
    }

    /// Atalho para a porta padrao do NTP.
    pub fn na_porta_ntp(nome: &str, endereco: &str) -> Result<Self, ErroDeContrato> {
        Self::novo(nome, endereco, PORTA_NTP)
    }

    /// Nome logico da fonte, como aparece na resposta da API.
    #[must_use]
    pub fn nome(&self) -> &str {
        &self.nome
    }

    /// Host ou endereco IP literal (sem porta).
    #[must_use]
    pub fn endereco(&self) -> &str {
        &self.endereco
    }

    /// Porta UDP.
    #[must_use]
    pub const fn porta(&self) -> u16 {
        self.porta
    }

    /// Endpoint `host:porta` -- o texto que o `UdpSocket::connect` consome.
    ///
    /// Pre-condicao do consumidor: `endereco` precisa ser um IPv6 entre
    /// colchetes se contiver `:`. IPv6 e' o caso comum no NTP.br (todos os
    /// servidores stratum 1 tem IPv6), entao o erro aqui seria silencioso e
    /// recorrente.
    #[must_use]
    pub fn endpoint(&self) -> String {
        if self.endereco.contains(':') {
            format!("[{}]:{}", self.endereco, self.porta)
        } else {
            format!("{}:{}", self.endereco, self.porta)
        }
    }
}

impl fmt::Display for AlvoDeConsulta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.nome, self.endpoint())
    }
}

/// Os servidores do NTP.br publicados em <https://ntp.br/>.
///
/// Lista de *fallback*, nao de verdade unica: o Observatorio Nacional e' a
/// fonte, mas a API precisa sobreviver a um servidor fora do ar -- e a
/// politica de resiliencia faz o resto.
///
/// Os enderecos sao os IPv4 de preferencia porque sao os que funcionam em
/// qualquer rede domestica; os IPv6 vem do mesmo site.
#[must_use]
pub fn alvos_do_ntp_br() -> Vec<AlvoDeConsulta> {
    [
        ("a.ntp.br", "200.160.0.8"),
        ("b.ntp.br", "200.189.40.8"),
        ("c.ntp.br", "200.192.232.8"),
        ("d.st1.ntp.br", "200.20.186.76"),
        ("gps.nu.ntp.br", "200.160.7.193"),
        ("gps.jd.ntp.br", "200.160.7.197"),
    ]
    .into_iter()
    .filter_map(|(nome, ip)| AlvoDeConsulta::na_porta_ntp(nome, ip).ok())
    .collect()
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::exigir;
    use crate::fixture::alvo_valido;

    #[test]
    fn ipv6_e_formatado_entre_colchetes() {
        let alvo = alvo_valido("st1", "2001:12ff::8", "[2001:12ff::8]:123");
        exigir!(
            alvo.endpoint() == "[2001:12ff::8]:123",
            "endpoint IPv6 com colchetes",
            "{}",
            alvo.endpoint()
        );
    }

    #[test]
    fn ipv4_nao_recebe_colchetes() {
        let alvo = alvo_valido("a", "200.160.0.8", "200.160.0.8:123");
        exigir!(
            alvo.endpoint() == "200.160.0.8:123",
            "endpoint IPv4 cru",
            "{}",
            alvo.endpoint()
        );
    }

    #[test]
    fn porta_zero_e_rejeitada() {
        let r = AlvoDeConsulta::novo("x", "1.2.3.4", 0);
        exigir!(r.is_err(), "pre-condicao: porta 0", "{r:?}");
    }

    #[test]
    fn nome_com_espaco_e_rejeitado() {
        let r = AlvoDeConsulta::na_porta_ntp(" a ", "1.2.3.4");
        exigir!(r.is_err(), "pre-condicao: nome aparado", "{r:?}");
    }

    #[test]
    fn a_lista_do_ntp_br_nao_tem_erros_de_construcao() {
        let alvos = alvos_do_ntp_br();
        exigir!(!alvos.is_empty(), "lista nao vazia", "vazia");
        for alvo in &alvos {
            exigir!(alvo.porta() == 123, "porta NTP", "{}", alvo);
        }
    }
}