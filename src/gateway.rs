use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use hyper::{body::HttpBody, Body, Client, Request};
use igd::PortMappingProtocol;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;
use tokio::time::timeout;
use xmltree::Element;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
pub const LEASE_SECONDS: u32 = 3600;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub destination: SocketAddrV4,
    pub description: String,
}

#[async_trait]
pub trait Router: Send + Sync {
    fn local_ip(&self) -> Ipv4Addr;
    async fn external_ip(&self) -> Result<Ipv4Addr>;
    async fn mapping(&self, protocol: PortMappingProtocol, port: u16) -> Result<Option<Mapping>>;
    async fn add(
        &self,
        protocol: PortMappingProtocol,
        port: u16,
        destination: SocketAddrV4,
        description: &str,
    ) -> Result<()>;
    async fn remove(&self, protocol: PortMappingProtocol, port: u16) -> Result<()>;
}

pub struct NetworkRouter {
    gateway: igd::aio::Gateway,
    local_ip: Ipv4Addr,
}

impl NetworkRouter {
    pub async fn discover() -> Result<Self> {
        let gateway = timeout(
            Duration::from_secs(5),
            igd::aio::search_gateway(igd::SearchOptions {
                timeout: Some(Duration::from_secs(3)),
                ..Default::default()
            }),
        )
        .await
        .context("Router discovery timed out. Check that UPnP is enabled on your router.")?
        .context("Could not find a UPnP router on this network")?;
        // Choose the interface used to reach this router, including multi-NIC/VPN systems.
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.connect(gateway.addr)?;
        let local_ip = match socket.local_addr()? {
            SocketAddr::V4(address) => *address.ip(),
            _ => bail!("The router requires an IPv4 connection"),
        };
        Ok(Self { gateway, local_ip })
    }
}

#[async_trait]
impl Router for NetworkRouter {
    fn local_ip(&self) -> Ipv4Addr {
        self.local_ip
    }
    async fn external_ip(&self) -> Result<Ipv4Addr> {
        let address = timeout(REQUEST_TIMEOUT, self.gateway.get_external_ip())
            .await
            .context("External address lookup timed out")??;
        if address.is_unspecified() {
            bail!("The router has no external IPv4 address yet");
        }
        Ok(address)
    }
    async fn mapping(&self, protocol: PortMappingProtocol, port: u16) -> Result<Option<Mapping>> {
        // igd exposes enumeration but not GetSpecificPortMappingEntry. A specific
        // lookup avoids scanning every rule and lets cleanup check ownership.
        timeout(REQUEST_TIMEOUT, async {
            let body = format!("<?xml version=\"1.0\"?>\
                <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
                <s:Body><u:GetSpecificPortMappingEntry xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">\
                <NewRemoteHost></NewRemoteHost><NewExternalPort>{port}</NewExternalPort>\
                <NewProtocol>{protocol}</NewProtocol></u:GetSpecificPortMappingEntry></s:Body></s:Envelope>");
            let request = Request::post(self.gateway.to_string())
                .header("Content-Type", "text/xml; charset=\"utf-8\"")
                .header("SOAPAction", "\"urn:schemas-upnp-org:service:WANIPConnection:1#GetSpecificPortMappingEntry\"")
                .body(Body::from(body))?;
            let response = Client::new().request(request).await?;
            let success = response.status().is_success();
            let mut stream = response.into_body();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.data().await {
                let chunk = chunk?;
                if bytes.len() + chunk.len() > 65536 { bail!("Router response is too large"); }
                bytes.extend_from_slice(&chunk);
            }
            parse_mapping(&bytes, success)
        }).await.context("Checking the existing mapping timed out")?
    }
    async fn add(
        &self,
        protocol: PortMappingProtocol,
        port: u16,
        destination: SocketAddrV4,
        description: &str,
    ) -> Result<()> {
        timeout(
            REQUEST_TIMEOUT,
            self.gateway
                .add_port(protocol, port, destination, LEASE_SECONDS, description),
        )
        .await
        .context("Adding the mapping timed out; its state will be checked during cleanup")??;
        Ok(())
    }
    async fn remove(&self, protocol: PortMappingProtocol, port: u16) -> Result<()> {
        match timeout(REQUEST_TIMEOUT, self.gateway.remove_port(protocol, port))
            .await
            .context("Removing the mapping timed out")?
        {
            Ok(()) | Err(igd::RemovePortError::NoSuchPortMapping) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn find<'a>(element: &'a Element, name: &str) -> Option<&'a Element> {
    if element.name == name {
        return Some(element);
    }
    element
        .children
        .iter()
        .filter_map(|node| node.as_element())
        .find_map(|child| find(child, name))
}

fn field(element: &Element, name: &str) -> Result<String> {
    find(element, name)
        .and_then(|value| value.get_text())
        .map(|text| text.into_owned())
        .with_context(|| format!("Router response is missing {name}"))
}

fn parse_mapping(bytes: &[u8], success: bool) -> Result<Option<Mapping>> {
    let document = Element::parse(bytes).context("Invalid XML from router")?;
    if let Some(fault) = find(&document, "Fault") {
        let code = field(fault, "errorCode")?;
        if code == "714" {
            return Ok(None);
        }
        bail!("Cannot inspect existing mappings (UPnP error {code}); no forwarding rule will be overwritten");
    }
    if !success {
        bail!("Router refused to inspect the existing mapping");
    }
    let entry = find(&document, "GetSpecificPortMappingEntryResponse")
        .context("Unexpected mapping response from router")?;
    Ok(Some(Mapping {
        destination: SocketAddrV4::new(
            field(entry, "NewInternalClient")?.parse()?,
            field(entry, "NewInternalPort")?.parse()?,
        ),
        description: field(entry, "NewPortMappingDescription").unwrap_or_default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn understands_missing_mapping_and_rejects_other_faults() {
        assert_eq!(
            parse_mapping(
                b"<Envelope><Fault><errorCode>714</errorCode></Fault></Envelope>",
                false
            )
            .unwrap(),
            None
        );
        assert!(parse_mapping(
            b"<Envelope><Fault><errorCode>401</errorCode></Fault></Envelope>",
            false
        )
        .is_err());
        assert!(parse_mapping(b"<Envelope/>", true).is_err());
    }
    #[test]
    fn reads_namespaced_mapping_response() {
        let response = br#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>
            <u:GetSpecificPortMappingEntryResponse xmlns:u="urn:schemas-upnp-org:service:WANIPConnection:1">
            <NewInternalClient>192.168.1.4</NewInternalClient><NewInternalPort>8080</NewInternalPort>
            <NewPortMappingDescription>another app</NewPortMappingDescription>
            </u:GetSpecificPortMappingEntryResponse></s:Body></s:Envelope>"#;
        let mapping = parse_mapping(response, true).unwrap().unwrap();
        assert_eq!(mapping.destination.to_string(), "192.168.1.4:8080");
        assert_eq!(mapping.description, "another app");
    }

    #[tokio::test]
    async fn soap_round_trip_checks_ownership_and_requests_a_finite_lease() {
        use hyper::{
            service::{make_service_fn, service_fn},
            Response, Server,
        };
        use std::{
            collections::HashMap,
            convert::Infallible,
            sync::{Arc, Mutex},
        };
        let stored = Arc::new(Mutex::new(None::<Mapping>));
        let server_state = stored.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = Server::from_tcp(listener).unwrap().serve(make_service_fn(move |_| {
            let stored = server_state.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |request: Request<Body>| {
                    let stored = stored.clone();
                    async move {
                        let action = request.headers()["SOAPAction"].to_str().unwrap().to_owned();
                        let bytes = hyper::body::to_bytes(request.into_body()).await.unwrap();
                        let document = Element::parse(bytes.as_ref()).unwrap();
                        let (name, body, code) = if action.contains("GetSpecificPortMappingEntry") {
                            assert_eq!(field(&document, "NewProtocol").unwrap(), "TCP");
                            assert_eq!(field(&document, "NewExternalPort").unwrap(), "9000");
                            match &*stored.lock().unwrap() {
                                None => ("Fault", "<errorCode>714</errorCode>".to_string(), 500),
                                Some(mapping) => ("GetSpecificPortMappingEntryResponse", format!("<NewInternalClient>{}</NewInternalClient><NewInternalPort>{}</NewInternalPort><NewPortMappingDescription>{}</NewPortMappingDescription>", mapping.destination.ip(), mapping.destination.port(), mapping.description), 200),
                            }
                        } else if action.contains("AddPortMapping") {
                            assert_eq!(field(&document, "NewLeaseDuration").unwrap(), "3600");
                            *stored.lock().unwrap() = Some(Mapping {
                                destination: SocketAddrV4::new(field(&document, "NewInternalClient").unwrap().parse().unwrap(), field(&document, "NewInternalPort").unwrap().parse().unwrap()),
                                description: field(&document, "NewPortMappingDescription").unwrap(),
                            });
                            ("AddPortMappingResponse", String::new(), 200)
                        } else if action.contains("DeletePortMapping") {
                            *stored.lock().unwrap() = None;
                            ("DeletePortMappingResponse", String::new(), 200)
                        } else {
                            ("GetExternalIPAddressResponse", "<NewExternalIPAddress>203.0.113.3</NewExternalIPAddress>".to_string(), 200)
                        };
                        Ok::<_, Infallible>(Response::builder().status(code).body(Body::from(format!("<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{name} xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">{body}</u:{name}></s:Body></s:Envelope>"))).unwrap())
                    }
                }))
            }
        }));
        let task = tokio::spawn(server);
        let mut schema = HashMap::new();
        schema.insert(
            "AddPortMapping".into(),
            [
                "NewRemoteHost",
                "NewExternalPort",
                "NewProtocol",
                "NewInternalPort",
                "NewInternalClient",
                "NewEnabled",
                "NewPortMappingDescription",
                "NewLeaseDuration",
            ]
            .map(String::from)
            .to_vec(),
        );
        schema.insert(
            "DeletePortMapping".into(),
            ["NewRemoteHost", "NewExternalPort", "NewProtocol"]
                .map(String::from)
                .to_vec(),
        );
        let router = NetworkRouter {
            gateway: igd::aio::Gateway {
                addr: match address {
                    SocketAddr::V4(a) => a,
                    _ => unreachable!(),
                },
                root_url: String::new(),
                control_url: "/control".into(),
                control_schema_url: String::new(),
                control_schema: schema,
            },
            local_ip: Ipv4Addr::LOCALHOST,
        };
        let protocol = PortMappingProtocol::TCP;
        assert!(router.mapping(protocol, 9000).await.unwrap().is_none());
        let destination = "127.0.0.1:8080".parse().unwrap();
        router
            .add(protocol, 9000, destination, "our session")
            .await
            .unwrap();
        assert_eq!(
            router.mapping(protocol, 9000).await.unwrap().unwrap(),
            Mapping {
                destination,
                description: "our session".into()
            }
        );
        assert_eq!(
            router.external_ip().await.unwrap().to_string(),
            "203.0.113.3"
        );
        router.remove(protocol, 9000).await.unwrap();
        assert!(router.mapping(protocol, 9000).await.unwrap().is_none());
        assert!(stored.lock().unwrap().is_none());
        task.abort();
    }
}
