//! Wissensdatenbank: Verwaltung für Agenten und öffentliches Hilfe-Center im Portal.

use std::collections::HashMap;

use askama::Template;
use axum::{
    response::{Html, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;

use super::extract::{Form, Path, Query, State};
use super::*;
use crate::{
    auth::{MaybePortalUser, WebAgent},
    kb::{self, ArticleInput},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/kb", get(agent_index).post(create_article))
        .route("/kb/new", get(new_article))
        .route("/kb/categories", post(create_category))
        .route("/kb/{id}", get(edit_article).post(save_article))
        .route("/kb/{id}/delete", post(delete_article))
        .route("/portal/help", get(help_index))
        .route("/portal/help/suggest", get(help_suggest))
        .route("/portal/help/{id}", get(help_article))
        .route("/portal/help/{id}/vote", post(help_vote))
}

#[derive(Deserialize, Default)]
struct SearchQuery {
    q: Option<String>,
}

struct ArticleRow {
    id: i64,
    title: String,
    excerpt: String,
    category: String,
    published: bool,
    stats: String,
}

fn rows(articles: &[kb::Article], cats: &HashMap<i64, String>) -> Vec<ArticleRow> {
    articles
        .iter()
        .map(|a| ArticleRow {
            id: a.id,
            title: a.title.clone(),
            excerpt: kb::excerpt(&a.body, 140),
            category: a.category_id.and_then(|c| cats.get(&c).cloned()).unwrap_or_default(),
            published: a.status == "published",
            stats: format!("{} Aufrufe · 👍 {} · 👎 {}", a.views, a.helpful_yes, a.helpful_no),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Agenten
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "kb/index.html")]
struct AgentIndex {
    nav: Nav,
    q: String,
    rows: Vec<ArticleRow>,
    categories: Vec<Opt>,
}

async fn agent_index(State(st): State<AppState>, WebAgent(me): WebAgent, Query(q): Query<SearchQuery>) -> WebResult<Html<String>> {
    let query = q.q.unwrap_or_default();
    let cats = kb::categories(&st.db).await?;
    let map: HashMap<i64, String> = cats.iter().cloned().collect();
    let articles = kb::search(&st.db, &query, false, false, 200).await?;
    page(AgentIndex {
        nav: Nav::new(&me, "kb"),
        q: query,
        rows: rows(&articles, &map),
        categories: cats.into_iter().map(|(id, name)| Opt { value: id.to_string(), label: name, selected: false }).collect(),
    })
}

#[derive(Template)]
#[template(path = "kb/edit.html")]
struct EditPage {
    nav: Nav,
    id: i64,
    title: String,
    body: String,
    published: bool,
    category_opts: Vec<Opt>,
    public_url: String,
    stats: String,
}

async fn edit_page(db: &PgPool, me: &User, article: Option<kb::Article>) -> WebResult<Html<String>> {
    let cats = kb::categories(db).await?;
    let selected = article.as_ref().and_then(|a| a.category_id);
    page(EditPage {
        nav: Nav::new(me, "kb"),
        id: article.as_ref().map(|a| a.id).unwrap_or(0),
        title: article.as_ref().map(|a| a.title.clone()).unwrap_or_default(),
        body: article.as_ref().map(|a| a.body.clone()).unwrap_or_default(),
        published: article.as_ref().is_some_and(|a| a.status == "published"),
        category_opts: id_opts(cats.iter().map(|(id, n)| (*id, n.as_str())), selected, "– ohne Kategorie –"),
        public_url: article.as_ref().map(|a| kb::article_url(a.id)).unwrap_or_default(),
        stats: article
            .as_ref()
            .map(|a| format!("{} Aufrufe · {} fanden ihn hilfreich, {} nicht", a.views, a.helpful_yes, a.helpful_no))
            .unwrap_or_default(),
    })
}

async fn new_article(State(st): State<AppState>, WebAgent(me): WebAgent) -> WebResult<Html<String>> {
    edit_page(&st.db, &me, None).await
}

async fn edit_article(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let article = kb::get(&st.db, id).await?;
    edit_page(&st.db, &me, Some(article)).await
}

#[derive(Deserialize)]
struct ArticleForm {
    title: String,
    body: String,
    category_id: Option<String>,
    status: String,
}

impl ArticleForm {
    fn input(self) -> ArticleInput {
        ArticleInput { category_id: parse_id(&self.category_id), title: self.title, body: self.body, status: self.status }
    }
}

async fn create_article(State(st): State<AppState>, WebAgent(me): WebAgent, Form(f): Form<ArticleForm>) -> WebResult<Response> {
    let id = kb::save(&st.db, None, f.input(), me.id).await?;
    Ok(see_other(&format!("/kb/{id}")))
}

async fn save_article(State(st): State<AppState>, WebAgent(me): WebAgent, Path(id): Path<i64>, Form(f): Form<ArticleForm>) -> WebResult<Response> {
    kb::save(&st.db, Some(id), f.input(), me.id).await?;
    Ok(see_other(&format!("/kb/{id}")))
}

async fn delete_article(State(st): State<AppState>, WebAgent(_): WebAgent, Path(id): Path<i64>) -> WebResult<Response> {
    sqlx::query("DELETE FROM kb_articles WHERE id = $1").bind(id).execute(&st.db).await?;
    Ok(see_other("/kb"))
}

#[derive(Deserialize)]
struct CategoryForm {
    name: String,
}

async fn create_category(State(st): State<AppState>, WebAgent(_): WebAgent, Form(f): Form<CategoryForm>) -> WebResult<Response> {
    kb::create_category(&st.db, &f.name).await?;
    Ok(see_other("/kb"))
}

// ---------------------------------------------------------------------------
// Hilfe-Center (öffentlich)
// ---------------------------------------------------------------------------

struct PortalNav {
    name: String,
}

struct CategoryBlock {
    name: String,
    rows: Vec<ArticleRow>,
}

#[derive(Template)]
#[template(path = "portal/help.html")]
struct HelpIndex {
    nav: PortalNav,
    q: String,
    results: Vec<ArticleRow>,
    blocks: Vec<CategoryBlock>,
    searched: bool,
}

async fn help_index(State(st): State<AppState>, MaybePortalUser(me): MaybePortalUser, Query(q): Query<SearchQuery>) -> WebResult<Html<String>> {
    let query = q.q.unwrap_or_default();
    let cats = kb::categories(&st.db).await?;
    let map: HashMap<i64, String> = cats.iter().cloned().collect();
    let searched = !query.trim().is_empty();
    let (results, blocks) = if searched {
        (rows(&kb::search(&st.db, &query, true, false, 50).await?, &map), Vec::new())
    } else {
        let all = kb::search(&st.db, "", true, false, 500).await?;
        let mut blocks: Vec<CategoryBlock> = cats
            .iter()
            .map(|(id, name)| CategoryBlock {
                name: name.clone(),
                rows: rows(&all.iter().filter(|a| a.category_id == Some(*id)).cloned().collect::<Vec<_>>(), &map),
            })
            .filter(|b| !b.rows.is_empty())
            .collect();
        let rest: Vec<kb::Article> = all.iter().filter(|a| a.category_id.is_none()).cloned().collect();
        if !rest.is_empty() {
            blocks.push(CategoryBlock { name: "Weitere Artikel".into(), rows: rows(&rest, &map) });
        }
        (Vec::new(), blocks)
    };
    page(HelpIndex { nav: PortalNav { name: me.map(|u| u.name).unwrap_or_default() }, q: query, results, blocks, searched })
}

#[derive(Template)]
#[template(path = "portal/help_article.html")]
struct HelpArticle {
    nav: PortalNav,
    id: i64,
    title: String,
    body: String,
    category: String,
    updated: String,
    voted: bool,
}

#[derive(Deserialize, Default)]
struct VotedQuery {
    voted: Option<String>,
}

async fn help_article(
    State(st): State<AppState>,
    MaybePortalUser(me): MaybePortalUser,
    Path(id): Path<i64>,
    Query(q): Query<VotedQuery>,
) -> WebResult<Html<String>> {
    let a = kb::get(&st.db, id).await?;
    if a.status != "published" {
        return Err(AppError::NotFound.into());
    }
    if q.voted.is_none() {
        kb::count_view(&st.db, id).await?;
    }
    let cats: HashMap<i64, String> = kb::categories(&st.db).await?.into_iter().collect();
    page(HelpArticle {
        nav: PortalNav { name: me.map(|u| u.name).unwrap_or_default() },
        id: a.id,
        category: a.category_id.and_then(|c| cats.get(&c).cloned()).unwrap_or_default(),
        updated: fmt_dt(a.updated_at),
        title: a.title,
        body: a.body,
        voted: q.voted.is_some(),
    })
}

#[derive(Deserialize)]
struct VoteForm {
    helpful: String,
}

async fn help_vote(State(st): State<AppState>, Path(id): Path<i64>, Form(f): Form<VoteForm>) -> WebResult<Response> {
    kb::vote(&st.db, id, f.helpful == "yes").await?;
    Ok(see_other(&format!("/portal/help/{id}?voted=1")))
}

#[derive(Template)]
#[template(path = "portal/help_suggest.html")]
struct HelpSuggest {
    rows: Vec<ArticleRow>,
}

/// htmx-Fragment: Artikelvorschläge passend zum eingegebenen Betreff.
async fn help_suggest(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> WebResult<Html<String>> {
    let text = q.get("subject").or_else(|| q.get("q")).cloned().unwrap_or_default();
    let articles = if text.trim().chars().count() < 4 { Vec::new() } else { kb::search(&st.db, &text, true, true, 4).await? };
    page(HelpSuggest { rows: rows(&articles, &HashMap::new()) })
}
