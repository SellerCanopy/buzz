//! What an end-to-end test needs from a local relay: a community's people, an agent and a
//! channel in the relay's tables, and a way to remove them afterwards.

#![allow(dead_code)]

pub mod script_agent;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use buzz_hive_api::routes::path;
use buzz_hive_api::App;
use nostr::Keys;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

pub fn setting(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

pub fn pubkey_bytes(keys: &Keys) -> Vec<u8> {
    keys.public_key().to_bytes().to_vec()
}

/// A call to a route as `pubkey`, in process.
pub async fn call(app: &App, method: &str, pubkey: &str, body: Value) -> (StatusCode, Value) {
    let (token, _) = app.minter.mint(pubkey, app.community, None).unwrap();
    let request = Request::post(path(method))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let router = buzz_hive_api::finish(buzz_hive_api::routes(app), app.clone());
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

/// The rows a test adds to the relay's database. Names are random, so a test that dies
/// before [`World::cleanup`] leaves rows no later run trips over.
pub struct World {
    pub pool: sqlx::PgPool,
    pub relay_pool: sqlx::PgPool,
    pub community: Uuid,
    pub relay_url: String,
    /// A relay owner, so Admin in Beehive.
    pub owner: Keys,
    /// The Beehive account that posts.
    pub hive: Keys,
    /// An agent the owner owns.
    pub agent: Keys,
    pub agent_name: String,
    pub channel: Uuid,
}

impl World {
    pub async fn seed() -> Self {
        let database_url = std::env::var("BUZZ_TEST_DATABASE_URL")
            .expect("set BUZZ_TEST_DATABASE_URL to the relay's database");
        let relay_url = setting("BUZZ_TEST_RELAY_URL", "ws://localhost:3000");
        let host = setting("BUZZ_TEST_COMMUNITY_HOST", "localhost");
        let pool = buzz_hive_store::migrate::connect(&database_url)
            .await
            .expect("connect to the database");
        buzz_hive_store::migrate::migrate(&pool).await.unwrap();
        // The relay's own side of the database. `pool` pins `search_path` to `hive`, which
        // the relay's write-fence triggers on `public` cannot resolve from.
        let relay_pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect as the relay");
        let community = buzz_hive_store::relay_read::RelayRead::new(pool.clone())
            .community_for_host(&host)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no community has host {host}"));

        let (hive, owner, agent) = (Keys::generate(), Keys::generate(), Keys::generate());
        let tag = Uuid::new_v4().simple().to_string()[..8].to_owned();
        let world = Self {
            pool,
            relay_pool,
            community,
            relay_url,
            owner,
            hive,
            agent,
            agent_name: format!("e2e-agent-{tag}"),
            channel: Uuid::new_v4(),
        };
        let relay = &world.relay_pool;
        for (keys, name, owned) in [
            (&world.owner, format!("e2e-owner-{tag}"), false),
            (&world.hive, format!("e2e-hive-{tag}"), false),
            (&world.agent, world.agent_name.clone(), true),
        ] {
            sqlx::query(
                "INSERT INTO public.users (community_id, pubkey, display_name, agent_owner_pubkey) \
                 VALUES ($1, $2, $3, CASE WHEN $4 THEN $5 END)",
            )
            .bind(community)
            .bind(pubkey_bytes(keys))
            .bind(name)
            .bind(owned)
            .bind(pubkey_bytes(&world.owner))
            .execute(relay)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO public.relay_members (community_id, pubkey, role) VALUES ($1, $2, 'owner')",
        )
        .bind(community)
        .bind(world.owner.public_key().to_hex())
        .execute(relay)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO public.channels (id, community_id, name, created_by) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(world.channel)
        .bind(community)
        .bind(format!("e2e-{tag}"))
        .bind(pubkey_bytes(&world.owner))
        .execute(relay)
        .await
        .unwrap();
        for keys in [&world.owner, &world.hive, &world.agent] {
            sqlx::query(
                "INSERT INTO public.channel_members (community_id, channel_id, pubkey) \
                 VALUES ($1, $2, $3)",
            )
            .bind(community)
            .bind(world.channel)
            .bind(pubkey_bytes(keys))
            .execute(relay)
            .await
            .unwrap();
        }
        world
    }

    pub async fn cleanup(&self, card_title: &str) {
        let agent = self.agent.public_key().to_hex();
        let owner = self.owner.public_key().to_hex();
        let _ = sqlx::query(
            "DELETE FROM hive.cards WHERE community_id = $1 AND title = $2 AND created_by = ANY($3)",
        )
        .bind(self.community)
        .bind(card_title)
        .bind(vec![agent.clone(), owner.clone()])
        .execute(&self.pool)
        .await;
        let _ =
            sqlx::query("DELETE FROM hive.access_levels WHERE community_id = $1 AND pubkey = $2")
                .bind(self.community)
                .bind(&agent)
                .execute(&self.pool)
                .await;
        let _ = sqlx::query("DELETE FROM public.channels WHERE community_id = $1 AND id = $2")
            .bind(self.community)
            .bind(self.channel)
            .execute(&self.relay_pool)
            .await;
        let _ =
            sqlx::query("DELETE FROM public.relay_members WHERE community_id = $1 AND pubkey = $2")
                .bind(self.community)
                .bind(&owner)
                .execute(&self.relay_pool)
                .await;
        let _ =
            sqlx::query("DELETE FROM public.users WHERE community_id = $1 AND pubkey = ANY($2)")
                .bind(self.community)
                .bind(vec![
                    pubkey_bytes(&self.owner),
                    pubkey_bytes(&self.hive),
                    pubkey_bytes(&self.agent),
                ])
                .execute(&self.relay_pool)
                .await;
    }
}
