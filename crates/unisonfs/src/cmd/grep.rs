//! `unisonfs grep` — semantic search across the Unison brain.
//!
//! Mirror of `smfs grep` — supports both semantic (flagless) and literal grep.

use anyhow::Result;
use clap::Args as ClapArgs;
use unisonfs_core::api::SearchReq;
use unisonfs_core::config::credentials::resolve_api_url;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Search query (semantic) or regex pattern (with --literal).
    pub query: String,

    /// Path prefix to restrict the search to (e.g. /private/notes/).
    pub path: Option<String>,

    /// Use literal regex grep instead of semantic search.
    #[arg(long, short = 'F')]
    pub literal: bool,

    /// Maximum number of results (default 10 for semantic, 50 for literal).
    #[arg(long, short = 'k')]
    pub limit: Option<u32>,

    /// Filter by document kind (note, wiki_page, raw, log, index).
    #[arg(long)]
    pub kind: Vec<String>,

    #[arg(long, env = "UNISON_TOKEN")]
    pub token: Option<String>,

    #[arg(long, env = "UNISON_API_URL")]
    pub api_url: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let token = super::auth::resolve_token(args.token.as_deref())?;
    let api_url = resolve_api_url(args.api_url.as_deref());
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);

    if args.literal {
        // Literal regex grep via GET /v1/brain/grep
        let resp = client
            .grep(&args.query, true, args.limit.or(Some(50)))
            .await?;

        if resp.results.is_empty() {
            eprintln!("(no results)");
            return Ok(());
        }

        for doc in &resp.results {
            println!("{}: {}", doc.path, doc.title.as_deref().unwrap_or("(no title)"));
        }
    } else {
        // Semantic search via GET /v1/brain/search
        let req = SearchReq {
            q: args.query.clone(),
            k: args.limit.or(Some(10)),
            kind: args.kind.clone(),
            memory_type: None,
            as_of: None,
        };

        let resp = client.search(&req).await?;

        if resp.results.is_empty() {
            eprintln!("(no results for \"{}\")", args.query);
            return Ok(());
        }

        for result in &resp.results {
            let path = &result.doc.path;
            let title = result.doc.title.as_deref().unwrap_or("(no title)");
            let score = result.score;
            if let Some(highlight) = &result.highlight {
                println!("{path} [{score:.3}] — {title}\n  {highlight}");
            } else {
                println!("{path} [{score:.3}] — {title}");
            }
        }
    }

    Ok(())
}
