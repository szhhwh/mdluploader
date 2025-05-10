use anyhow::{Ok, Result};
use opendal::layers::ConcurrentLimitLayer;
use opendal::layers::LoggingLayer;
use opendal::services;
use opendal::Operator;

/// Represents the configuration for an AWS S3 storage service.
/// 
/// This struct holds the necessary credentials and configuration details
/// required to interact with an AWS S3 bucket.
#[derive(Default)]
pub struct AwsS3 {
    /// The name of the S3 bucket.
    bucket: String,
    /// The access key for the S3 bucket.
    ak: String,
    /// The secret key for the S3 bucket.
    sk: String,
    /// The AWS region where the S3 bucket is located.
    region: String,
    /// The endpoint URL for the S3 service.
    ep: String,
    /// The root directory within the S3 bucket.
    root: String,
}

impl AwsS3 {
    /// Creates a new `AwsS3` instance with the provided configuration.
    /// 
    /// # Arguments
    /// 
    /// * `bucket` - The name of the S3 bucket.
    /// * `ak` - The access key for the S3 bucket.
    /// * `sk` - The secret key for the S3 bucket.
    /// * `region` - The AWS region where the S3 bucket is located.
    /// * `ep` - The endpoint URL for the S3 service.
    /// * `root` - The root directory within the S3 bucket.
    /// 
    /// # Returns
    /// 
    /// A new instance of `AwsS3`.
    pub fn new(
        bucket: impl Into<String>,
        ak: impl Into<String>,
        sk: impl Into<String>,
        region: impl Into<String>,
        ep: impl Into<String>,
        root: impl Into<String>,
    ) -> AwsS3 {
        AwsS3 {
            bucket: bucket.into(),
            ak: ak.into(),
            sk: sk.into(),
            region: region.into(),
            ep: ep.into(),
            root: root.into(),
        }
    }

    /// Builds an `Operator` instance for interacting with the S3 bucket.
    /// 
    /// This method configures the S3 service with the provided credentials
    /// and settings, and applies additional layers for logging and concurrency control.
    /// 
    /// # Returns
    /// 
    /// A `Result` containing the configured `Operator` on success, or an error on failure.
    pub fn build(&self) -> Result<Operator> {
        let builder = services::S3::default()
            .disable_config_load()
            .bucket(&self.bucket)
            .access_key_id(&self.ak)
            .secret_access_key(&self.sk)
            .region(&self.region)
            .root(&self.root)
            .endpoint(&self.ep);

        let op = Operator::new(builder)?
            .layer(LoggingLayer::default())
            .layer(ConcurrentLimitLayer::new(1024))
            .finish();

        Ok(op)
    }
}
