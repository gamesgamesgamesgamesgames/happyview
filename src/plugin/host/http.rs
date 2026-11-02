use super::{MAX_HTTP_REQUESTS, MAX_HTTP_RESPONSE_SIZE, MAX_HTTP_TOTAL_TRANSFER, ResourceUsage};
use serde::{Deserialize, Serialize};

/// Accepts both a JSON string and a byte array for the body field,
/// so plugins can send either `"body": "text"` or `"body": [1,2,3]`.
fn deserialize_body_flexible<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct BodyVisitor;
    impl<'de> de::Visitor<'de> for BodyVisitor {
        type Value = Option<Vec<u8>>;

        fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            f.write_str("a string, byte array, or null")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(Some(v.as_bytes().to_vec()))
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            Ok(Some(v.into_bytes()))
        }

        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(Some(v.to_vec()))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            let v: Vec<u8> =
                de::Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
            Ok(Some(v))
        }

        fn visit_some<D2: serde::Deserializer<'de>>(
            self,
            deserializer: D2,
        ) -> Result<Self::Value, D2::Error> {
            deserializer.deserialize_any(BodyVisitor)
        }
    }

    deserializer.deserialize_any(BodyVisitor)
}

/// Serialize response body as a UTF-8 string when valid, otherwise as a byte array.
/// This ensures plugins that declare `body: Option<String>` can deserialize the response.
fn serialize_body_as_string<S>(body: &[u8], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match core::str::from_utf8(body) {
        Ok(s) => serializer.serialize_str(s),
        Err(_) => serializer.serialize_bytes(body),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    #[serde(default, deserialize_with = "deserialize_body_flexible")]
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[serde(serialize_with = "serialize_body_as_string")]
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("Too many requests: {0} > {MAX_HTTP_REQUESTS}")]
    TooManyRequests(u32),
    #[error("Response too large: {0} > {MAX_HTTP_RESPONSE_SIZE}")]
    ResponseTooLarge(u64),
    #[error("Transfer limit exceeded: {0} > {MAX_HTTP_TOTAL_TRANSFER}")]
    TransferLimitExceeded(u64),
    #[error("Request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),
}

pub async fn http_request(
    usage: &mut ResourceUsage,
    req: HttpRequest,
) -> Result<HttpResponse, HttpError> {
    // Check request count limit
    usage.http_requests += 1;
    if usage.http_requests > MAX_HTTP_REQUESTS {
        return Err(HttpError::TooManyRequests(usage.http_requests));
    }

    // Build request
    let method = req.method.parse().unwrap_or(reqwest::Method::GET);
    // The plugin names this URL itself, so it goes through the client
    // `ALLOW_PRIVATE_HOSTS` reaches; see `crate::outbound`.
    let mut builder = crate::http_retry::plugin_client().request(method, &req.url);

    for (name, value) in &req.headers {
        builder = builder.header(name, value);
    }

    if let Some(body) = req.body {
        usage.http_bytes_transferred += body.len() as u64;
        builder = builder.body(body);
    }

    // Check transfer limit before sending
    if usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER {
        return Err(HttpError::TransferLimitExceeded(
            usage.http_bytes_transferred,
        ));
    }

    // Execute request
    let response = builder.send().await?;
    let status = response.status().as_u16();

    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();

    let body = response.bytes().await?;

    // Check response size
    if body.len() as u64 > MAX_HTTP_RESPONSE_SIZE {
        return Err(HttpError::ResponseTooLarge(body.len() as u64));
    }

    usage.http_bytes_transferred += body.len() as u64;
    if usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER {
        return Err(HttpError::TransferLimitExceeded(
            usage.http_bytes_transferred,
        ));
    }

    Ok(HttpResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_request_limit_check() {
        let mut usage = ResourceUsage {
            http_requests: MAX_HTTP_REQUESTS,
            ..Default::default()
        };

        // Verify the limit check would fail
        usage.http_requests += 1;
        assert!(usage.http_requests > MAX_HTTP_REQUESTS);
    }

    #[test]
    fn test_http_transfer_limit_check() {
        let mut usage = ResourceUsage {
            http_bytes_transferred: MAX_HTTP_TOTAL_TRANSFER,
            ..Default::default()
        };

        // Adding more would exceed limit
        usage.http_bytes_transferred += 1;
        assert!(usage.http_bytes_transferred > MAX_HTTP_TOTAL_TRANSFER);
    }

    #[test]
    fn test_http_response_struct() {
        let response = HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{}".to_vec(),
        };
        assert_eq!(response.status, 200);
        assert_eq!(response.headers.len(), 1);
    }
}
