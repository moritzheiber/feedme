use std::fmt;

#[derive(Debug)]
pub struct Config {
    pub api_key: String,
    pub database_url: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub enum ConfigError {
    MissingEmail,
    MissingPassword,
    InvalidPort(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingEmail => write!(f, "FEEDME_EMAIL is required"),
            ConfigError::MissingPassword => write!(f, "FEEDME_PASSWORD is required"),
            ConfigError::InvalidPort(v) => write!(f, "FEEDME_PORT is not a valid port: {v}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    pub fn from_vars<F>(get_var: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let email = get_var("FEEDME_EMAIL").ok_or(ConfigError::MissingEmail)?;
        let password = get_var("FEEDME_PASSWORD").ok_or(ConfigError::MissingPassword)?;
        let api_key = format!("{:x}", md5::compute(format!("{email}:{password}")));
        let database_url =
            get_var("FEEDME_DATABASE_URL").unwrap_or_else(|| "feedme.db".to_string());
        let host = get_var("FEEDME_HOST").unwrap_or_else(|| "0.0.0.0".to_string());
        let port = match get_var("FEEDME_PORT") {
            Some(p) => p.parse().map_err(|_| ConfigError::InvalidPort(p))?,
            None => 8080,
        };
        Ok(Self {
            api_key,
            database_url,
            host,
            port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn from_vars_with_required_only() {
        let vars = make_vars(&[
            ("FEEDME_EMAIL", "test@example.com"),
            ("FEEDME_PASSWORD", "secret"),
        ]);

        let config = Config::from_vars(vars).unwrap();

        let expected_key = format!("{:x}", md5::compute("test@example.com:secret"));
        assert_eq!(config.api_key, expected_key);
        assert_eq!(config.database_url, "feedme.db");
        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn from_vars_with_all_vars() {
        let vars = make_vars(&[
            ("FEEDME_EMAIL", "user@test.com"),
            ("FEEDME_PASSWORD", "pass"),
            ("FEEDME_DATABASE_URL", "/tmp/test.db"),
            ("FEEDME_HOST", "127.0.0.1"),
            ("FEEDME_PORT", "3000"),
        ]);

        let config = Config::from_vars(vars).unwrap();

        let expected_key = format!("{:x}", md5::compute("user@test.com:pass"));
        assert_eq!(config.api_key, expected_key);
        assert_eq!(config.database_url, "/tmp/test.db");
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn from_vars_missing_email() {
        let vars = make_vars(&[("FEEDME_PASSWORD", "secret")]);
        let err = Config::from_vars(vars).unwrap_err();
        assert!(matches!(err, ConfigError::MissingEmail));
    }

    #[test]
    fn from_vars_missing_password() {
        let vars = make_vars(&[("FEEDME_EMAIL", "test@example.com")]);
        let err = Config::from_vars(vars).unwrap_err();
        assert!(matches!(err, ConfigError::MissingPassword));
    }

    #[test]
    fn from_vars_invalid_port() {
        let vars = make_vars(&[
            ("FEEDME_EMAIL", "test@example.com"),
            ("FEEDME_PASSWORD", "secret"),
            ("FEEDME_PORT", "not_a_number"),
        ]);
        let err = Config::from_vars(vars).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidPort(_)));
    }
}
