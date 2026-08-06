use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;

pub type ResolveFuture = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, String>> + Send>>;

/// DNS resolution seam. The product resolver uses the system resolver;
/// tests inject static tables to exercise rebinding scenarios without
/// touching the network.
pub trait DnsResolver: Send + Sync + 'static {
    fn lookup(&self, host: &str, port: u16) -> ResolveFuture;
}

/// System resolver backed by `getaddrinfo`.
#[derive(Debug, Clone, Default)]
pub struct SystemResolver;

impl DnsResolver for SystemResolver {
    fn lookup(&self, host: &str, port: u16) -> ResolveFuture {
        let host = host.to_string();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|error| format!("failed to resolve `{host}`: {error}"))?
                .map(|address: SocketAddr| address.ip())
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(format!("no addresses found for `{host}`"));
            }
            Ok(addresses)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct EmptyResolver;

    impl DnsResolver for EmptyResolver {
        fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
            Box::pin(async { Ok(vec![]) })
        }
    }

    #[tokio::test]
    async fn system_resolver_reports_empty_results_as_errors() {
        let error = SystemResolver.lookup("does-not-exist.invalid", 80).await;
        match error {
            Ok(_) => panic!("NXD names must not resolve"),
            Err(message) => assert!(!message.is_empty()),
        }
    }

    #[tokio::test]
    async fn resolver_contract_round_trips_ips() {
        struct OneResolver;
        impl DnsResolver for OneResolver {
            fn lookup(&self, _host: &str, _port: u16) -> ResolveFuture {
                Box::pin(async { Ok(vec![IpAddr::from([93, 184, 216, 34])]) })
            }
        }
        assert_eq!(OneResolver.lookup("x.example", 443).await.unwrap().len(), 1);
        assert!(
            EmptyResolver
                .lookup("y.example", 443)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
