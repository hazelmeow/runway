use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use thiserror::Error;

const API_PATH: &str = "https://assetdelivery.roblox.com/v1/asset/?id=";

static REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"https?:\/\/www.roblox.com\/asset\/\?id=(\d+)"#).unwrap());

#[derive(Debug)]
pub struct AssetDelivery {
    client: Client,
}

impl AssetDelivery {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    pub async fn get_texture<S: AsRef<str>>(&self, id: S) -> Result<String, ApiError> {
        let mut url = API_PATH.to_string();
        url.push_str(id.as_ref());
        let data = self.client.get(url).send().await?.text().await?;

        let Some(caps) = REGEX.captures(&data) else {
			log::trace!("Regex did not match response: {}", data);

			return Err(ApiError::NoRegexMatch)
		};

        let id = &caps[1];
        Ok(id.to_string())
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Failed to parse Roblox asset delivery API response")]
    NoRegexMatch,

    #[error(transparent)]
    Reqwest {
        #[from]
        source: reqwest::Error,
    },
}
