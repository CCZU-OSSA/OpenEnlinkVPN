pub mod auth;
pub(crate) mod trust;
pub mod vpn;

#[cfg(test)]
mod test {

    use crate::{auth::AuthorizationStore, vpn::EnlinkVpn};

    #[tokio::test]
    async fn test_vpn_stablity() {
        let auth = AuthorizationStore::default();

        let vpn = EnlinkVpn::connect(auth).await.unwrap();
        vpn.authorize().await.unwrap();
        println!("{:?}", vpn.is_authorize_ok().await);
        println!("{:?}", vpn.virtual_address().await);
    }
}
