use crate::config::SubsonicConfig;
use crate::models::RequestContext;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub password: Option<String>,
    pub token: Option<String>,
    pub salt: Option<String>,
}

impl Credentials {
    pub fn from_request(request: &RequestContext) -> Self {
        Self {
            username: query_value(request, "u").unwrap_or_default().to_string(),
            password: query_value(request, "p").map(ToString::to_string),
            token: query_value(request, "t").map(ToString::to_string),
            salt: query_value(request, "s").map(ToString::to_string),
        }
    }

    pub fn matches(&self, config: &SubsonicConfig) -> bool {
        if self.username != config.username {
            return false;
        }

        if let Some(password) = &self.password {
            return matches_password(password, &config.password);
        }

        match (&self.token, &self.salt) {
            (Some(token), Some(salt)) => {
                let expected = format!("{:x}", md5::compute(format!("{}{salt}", config.password)));
                token.eq_ignore_ascii_case(&expected)
            }
            _ => false,
        }
    }
}

fn matches_password(provided: &str, expected: &str) -> bool {
    if provided == expected {
        return true;
    }

    if let Some(hex) = provided.strip_prefix("enc:") {
        return decode_hex_utf8(hex).as_deref() == Ok(expected);
    }

    false
}

fn decode_hex_utf8(hex: &str) -> Result<String, ()> {
    if hex.len() % 2 != 0 {
        return Err(());
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let chars: Vec<char> = hex.chars().collect();
    for idx in (0..chars.len()).step_by(2) {
        let hi = chars[idx].to_digit(16).ok_or(())?;
        let lo = chars[idx + 1].to_digit(16).ok_or(())?;
        bytes.push(((hi << 4) | lo) as u8);
    }

    String::from_utf8(bytes).map_err(|_| ())
}

fn query_value<'a>(request: &'a RequestContext, key: &str) -> Option<&'a str> {
    request
        .query
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}
