//! handles http requests for the [matrix_sdk::Client] to allow us to record
//! metrics
use std::{convert::TryFrom, time::Duration};

use anyhow::{Context, Result};
use matrix_sdk::{
	async_trait, bytes::Bytes, config::RequestConfig, reqwest, ruma::UserId, HttpError, HttpSend,
};
use prometheus::{HistogramVec, IntCounterVec};

#[derive(Debug, Clone)]
/// prometheus meters for the matrix-sdk http client
struct Metrics {
	/// total number of http requests
	http_requests: IntCounterVec,
	/// total number of failed http requests
	http_requests_failed: IntCounterVec,
	/// time spent by http requests
	http_request_duration: HistogramVec,
}

impl Metrics {
	/// construct and register prometheus meters
	pub fn new(bot_id: &UserId) -> Result<Self, prometheus::Error> {
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
		)?;

		let http_requests_failed = register_int_counter_vec!(
			opts!(
				"requests_failed",
				"number of failed http requests",
				labels! {"bot" => bot_id.as_str()}
			)
			.namespace("howler")
			.subsystem("bot_http_client"),
			&["host", "status_code"]
		)?;

		let http_request_duration = register_histogram_vec!(
			histogram_opts!(
				"request_duration_seconds",
				"total time of a http request in seconds",
				exponential_buckets(0.01, 2., 12)?,
				labels! {"bot".to_owned() => bot_id.to_string()}
			)
			.subsystem("bot_http_client")
			.namespace("howler"),
			&["host"]
		)?;

		Ok(Self { http_requests, http_requests_failed, http_request_duration })
	}
}

#[derive(Debug, Clone)]
/// Custom http client for matrix_sdk. Doesn't do backoffs themself because we
/// want to do that in a central place for all bots.
pub struct Client {
	/// http client
	client: reqwest::Client,
	/// prometheus meters for http requests
	metrics: Metrics,
}

impl Client {
	/// construct http client
	pub fn new(bot_id: &UserId) -> Result<Self> {
		Ok(Self {
			client: reqwest::Client::builder()
				.timeout(Duration::from_secs(10))
				.build()
				.context("failed to build reqwest client")?,
			metrics: Metrics::new(bot_id).context("failed to register prometheus meters")?,
		})
	}

	/// convert reqwest response into [http::Response<Bytes>] for the matrix-sdk
	async fn response_to_http_response(
		&self,
		mut response: reqwest::Response,
	) -> Result<http::Response<Bytes>, HttpError> {
		let status = response.status();

		let mut http_builder = http::Response::builder().status(status);
		#[allow(clippy::expect_used)]
		let headers = http_builder.headers_mut().expect("can't get the response builder headers");

		for (k, v) in response.headers_mut().drain() {
			if let Some(key) = k {
				headers.insert(key, v);
			}
		}

		let body = response.bytes().await?;

		#[allow(clippy::expect_used)]
		Ok(http_builder.body(body).expect("can't construct a response using the given body"))
		// Convert the reqwest response to a http one.
	}
}

#[async_trait]
impl HttpSend for Client {
	async fn send_request(
		&self,
		request: http::Request<Bytes>,
		_: RequestConfig,
	) -> Result<http::Response<Bytes>, HttpError> {
		#[allow(clippy::expect_used)]
		let _timer = self
			.metrics
			.http_request_duration
			.with_label_values(&[request.uri().host().expect("http request without host string")])
			.start_timer();

		#[allow(clippy::expect_used)]
		self.metrics
			.http_requests
			.with_label_values(&[request.uri().host().expect("http request without host string")])
			.inc();

		let response = self.client.execute(reqwest::Request::try_from(request)?).await?;

		#[allow(clippy::expect_used)]
		if !response.status().is_success() {
			self.metrics
				.http_requests_failed
				.with_label_values(&[
					response.url().host_str().expect("http request without host string"),
					response.status().as_str(),
				])
				.inc();
		}

		Ok(self.response_to_http_response(response).await?)
	}
}
