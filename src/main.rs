mod model;

use std::{collections::HashSet, error::Error, path::PathBuf, sync::Arc};

use async_recursion::async_recursion;
use clap::Parser;
use linkify::{LinkFinder, LinkKind};
use model::{Record, SiteURLNode};
use reqwest::Client;
use surrealdb::{engine::local::{Db, SurrealKV}, Surreal};
use tracing::{debug, error, info, trace};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "findconn",
    author = "HyperCodec",
    about = "Finds all connected links (at least ones accessible via GET request) and "
)]
struct Cli {
    #[arg(short, long, help = "The output DB directory")]
    output: PathBuf,

    #[arg(short, long, help = "The initial site to parse")]
    url: String,
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
    
    let db_name = format!("tree-{}", args.url);
    info!("Using db name: {db_name:?}");

    db.use_ns("findconn").use_db(db_name).await?;

    trace!("SurrealDB setup successfully");

    let mut link_finder = LinkFinder::new();
    link_finder.kinds(&[LinkKind::Url]);

    let state = Arc::new(AppState {
        db,
        request_client: Client::new(),
        link_finder,
    });

    info!("Scraping for site linkages ...");
    discover_sites(None, args.url, state).await?;
    info!("Done scraping connections");

    Ok(())
}

#[async_recursion]
async fn discover_sites(source: Option<SiteURLNode>, url: String, state: Arc<AppState>) -> anyhow::Result<()> {
    // check to make sure site isnt already there.
    let mut res = state.db
        .query("SELECT id FROM site WHERE url = $url")
        .bind(("url", url.clone()))
        .await?;

    let findings: Vec<Record> = res.take(0)?;
    if !findings.is_empty() {
        debug!("Found url that was already searched, skipping");
        return Ok(());
    }

    let mut obj: SiteURLNode = SiteURLNode::new(url.clone());
    let newsource: Record = state.db
        .create("site")
        .content(obj.clone())
        .await?
        .unwrap();
    obj.id = Some(newsource.id.clone());

    if let Some(source) = source {
        //println!("relating {:?} to {:?}", source.id.clone().unwrap().to_raw(), newsource.id.to_raw());
        state.db
            .query("RELATE $sourceid->ref->$currentid")
            .bind(("sourceid", source.id.unwrap().to_raw()))
            .bind(("currentid", newsource.id.to_raw()))
            .await?;
    }

    // get content
    let req = state.request_client
        .get(url)
        .build()?;

    let res = state.request_client.execute(req).await?;

    let content = res.text().await?;
    
    let mut handles = Vec::new();
    let links: HashSet<String> = state.link_finder.links(&content)
        .map(|link| link.as_str().to_owned())
        .collect();
    for link in links {
        let url2 = link.as_str().to_owned();
        info!("Found url: {url2}");
    
        let state2 = state.clone();
        let obj2 = obj.clone();
        handles.push(tokio::spawn(async move {
            discover_sites(Some(obj2), url2, state2).await
        }));
    }

    for h in handles {
        let result = h.await?;
        if result.is_err() {
            // i don't think there's any fatal errors here so it's ok to just log it instead of bubbling it up.
            error!("{:?}", result.unwrap_err());
        }
    }

    Ok(())
}
