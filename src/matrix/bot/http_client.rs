//! handles http requests for the [matrix_sdk::Client] to allow us to record metrics
use std::{convert::TryFrom, time::Duration};

use matrix_sdk::{
    async_trait,
    bytes::Bytes,
    config::RequestConfig,
    reqwest,
    ruma::{api::exports::http, UserId},
    HttpError, HttpSend,
};
use prometheus::{HistogramVec, IntCounterVec};

#[derive(Debug, Clone)]
struct Metrics {
    http_requests: IntCounterVec,
    http_requests_failed: IntCounterVec,
    http_request_duration: HistogramVec,
}

impl Metrics {
    pub fn new(bot_id: &UserId) -> Self {
        use prometheus::{
            exponential_buckets, histogram_opts, labels, opts, register_histogram_vec,
            register_int_counter_vec,
        };

        let http_requests = register_int_counter_vec!(
            opts!(
                "requests_total",
                "total number of http requests",
                labels! {"bot" => bot_id.as_str()}
            )
            .namespace("howler")
            .subsystem("bot_http_client"),
            &["host"]
        )
        .unwrap();

        let http_requests_failed = register_int_counter_vec!(
            opts!(
                "requests_failed",
                "number of failed http requests",
                labels! {"bot" => bot_id.as_str()}
            )
            .namespace("howler")
            .subsystem("bot_http_client"),
            &["host", "status_code"]
        )
        .unwrap();

        let http_request_duration = register_histogram_vec!(
            histogram_opts!(
                "request_duration_seconds",
                "total time of a http request in seconds",
                exponential_buckets(0.01, 2., 12).unwrap(),
                labels! {"bot".to_string() => bot_id.to_string()}
            )
            .subsystem("bot_http_client")
            .namespace("howler"),
            &["host"]
        )
        .unwrap();

        Self {
            http_requests,
            http_requests_failed,
            http_request_duration,
        }
    }
}

#[derive(Debug, Clone)]
/// Custom client for matrix_sdk. Doesn't do backoffs themself because we want to do that in a central place for all bots.
pub struct Client {
    client: reqwest::Client,
    metrics: Metrics,
}

impl Client {
    pub fn new(bot_id: &UserId) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            metrics: Metrics::new(bot_id),
        }
    }

    async fn response_to_http_response(
        &self,
        mut response: reqwest::Response,
    ) -> Result<http::Response<Bytes>, HttpError> {
        let status = response.status();

        let mut http_builder = http::Response::builder().status(status);
        let headers = http_builder
            .headers_mut()
            .expect("Can't get the response builder headers");

        for (k, v) in response.headers_mut().drain() {
            if let Some(key) = k {
                headers.insert(key, v);
            }
        }

        let body = response.bytes().await?;

        Ok(http_builder
            .body(body)
            .expect("Can't construct a response using the given body")) // Convert the reqwest response to a http one.
    }
}

#[async_trait]
impl HttpSend for Client {
    async fn send_request(
        &self,
        request: http::Request<Bytes>,
        _: RequestConfig,
    ) -> Result<http::Response<Bytes>, HttpError> {
        self.metrics
            .http_requests
            .with_label_values(&[request.uri().host().unwrap()])
            .inc();

        let _timer = self
            .metrics
            .http_request_duration
            .with_label_values(&[request.uri().host().unwrap()])
            .start_timer();

        let response = self
            .client
            .execute(reqwest::Request::try_from(request)?)
            .await?;

        if !response.status().is_success() {
            self.metrics
                .http_requests_failed
                .with_label_values(&[
                    response.url().host_str().unwrap(),
                    response.status().as_str(),
                ])
                .inc();
        }

        Ok(self.response_to_http_response(response).await?)
    }
}
