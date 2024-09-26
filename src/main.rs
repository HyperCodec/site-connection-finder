mod model;

use std::{collections::HashSet, error::Error, path::PathBuf, sync::Arc};

use async_recursion::async_recursion;
use clap::Parser;
use linkify::{LinkFinder, LinkKind};
use model::{Record, Relation, SiteURLNode};
use reqwest::{header::CONTENT_TYPE, Client, Proxy};
use surrealdb::{engine::local::{Db, SurrealKV}, Surreal};
use tracing::{debug, error, info, trace};
use tracing_subscriber::EnvFilter;
use url::Url;

#[derive(Parser)]
#[command(
    name = "findconn",
    author = "HyperCodec",
    about = "Finds all connected links (at least ones accessible via GET request) and dumps them into a SurrealDB database."
)]
struct Cli {
    #[arg(short, long, help = "The output DB directory")]
    output: PathBuf,

    #[arg(short, long, help = "The initial site to parse")]
    url: String,

    #[arg(short, long, help = "The namespace to use in surrealDB", default_value = "findconn")]
    ns: String,

    #[arg(short, long, help = "The database name to use in surrealDB. Defaults to `tree-{url}` if not specified.")]
    db: Option<String>,

    #[arg(short, long, help = "The table where the urls will be stored", default_value = "site")]
    table: String,

    #[arg(short, long, help = "A proxy so you dont blast your home IP across the web (recommended)")]
    proxy: Option<String>,

    #[arg(short, long, help = "The table used for relations", default_value = "containslink")]
    relate_table: String,
}

struct AppState {
    db: Surreal<Db>,
    request_client: Client,
    link_finder: LinkFinder,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
            .unwrap_or(EnvFilter::from("INFO"))
        )
        .init();

    let args = Cli::parse();

    trace!("Setting up SurrealDB");
    let db = Surreal::new::<SurrealKV>(args.output).await?;
    
    let db_name = if let Some(db_name) = args.db {
        db_name
    } else {
        format!("tree-{}", args.url)
    };
    info!("Using db name: {db_name:?}");

    db.use_ns(args.ns).use_db(db_name).await?;

    trace!("SurrealDB setup successfully");

    let mut link_finder = LinkFinder::new();
    link_finder.kinds(&[LinkKind::Url]);

    let mut client_builder = Client::builder();

    if let Some(proxy) = args.proxy {
        client_builder = client_builder.proxy(Proxy::all(proxy)?);
    }

    let state = Arc::new(AppState {
        db,
        request_client: client_builder.build()?,
        link_finder,
    });

    info!("Scraping for site linkages ...");
    discover_sites(args.table, args.relate_table, None, args.url, state).await?;
    info!("Done scraping connections");

    Ok(())
}

#[async_recursion]
async fn discover_sites(table: String, relate_table: String, source: Option<SiteURLNode>, url: String, state: Arc<AppState>) -> anyhow::Result<()> {
    // helps with comparing string urls
    let url = Url::parse(&url)?.to_string();

    // check to make sure site isnt already there.
    let mut res = state.db
        .query("SELECT id FROM type::table($table) WHERE url = $url LIMIT 1")
        .bind(("url", url.clone()))
        .bind(("table", table.clone()))
        .await?;

    let findings: Vec<Record> = res.take(0)?;
    if !findings.is_empty() {
        debug!("Found url that was already searched, skipping");
        return Ok(());
    }

    let mut obj: SiteURLNode = SiteURLNode::new(url.clone());
    let newsource: Record = state.db
        .create(&table)
        .content(obj.clone())
        .await?
        .unwrap();
    obj.id = Some(newsource.id.clone());

    if let Some(source) = source {
        let mut res = state.db
            .query(format!("RELATE $sourceid->{relate_table}->$currentid"))
            .bind(("sourceid", source.id.unwrap()))
            .bind(("currentid", newsource.id))
            .await?;

        let _: Option<Relation> = res.take(0)?;
    }

    // get content
    let req = state.request_client
        .get(&url)
        .build()?;

    let res = state.request_client.execute(req).await?;

    let content_type = res.headers().get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    const APPLICATION_TEXT_HEADERS: [&str; 5] = [
        "application/json",
        "application/xml",
        "application/javascript",
        "application/ld+json",
        "application/xtml+xml",
    ];
    if !content_type.starts_with("text/") && !APPLICATION_TEXT_HEADERS.contains(&content_type) {
        debug!("Url {url:?} is content-type {content_type:?}, expected \"text/*\". Ignoring.");
        return Ok(());
    }

    let content = res.text().await?;
    
    let mut handles = Vec::new();
    let links: HashSet<String> = state.link_finder.links(&content)
        .map(|link| link.as_str().to_owned())
        .collect();
    for link in links {
        info!("Found url: {link}");
    
        let state2 = state.clone();
        let obj2 = obj.clone();
        let table2 = table.clone();
        let relate_table2 = relate_table.clone();
        handles.push(tokio::spawn(async move {
            discover_sites(table2, relate_table2, Some(obj2), link, state2).await
        }));
    }

    for h in handles {
        let result = h.await?;
        if let Err(err) = result {
            // i don't think there's any fatal errors here so it's ok to just log it instead of bubbling it up.
            error!("{err:?}");
        }
    }

    Ok(())
}
