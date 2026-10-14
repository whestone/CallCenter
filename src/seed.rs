//! Startdaten für eine leere Datenbank: Admin, Agent, Gruppen, Views, Makros,
//! Triggers, SLA und ein paar Beispiel-Tickets.

use serde_json::json;
use sqlx::PgPool;

use crate::{
    auth,
    models::User,
    service::{self, CommentChange, NewTicket, TicketChanges, UserInput},
};

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into())
}

pub async fn run(db: &PgPool) -> anyhow::Result<()> {
    // Der technische Automations-Benutzer (Migration) zählt nicht.
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE password_hash IS NOT NULL")
        .fetch_one(db)
        .await?;
    if users > 0 {
        return Ok(());
    }
    tracing::info!("Leere Datenbank – lege Startdaten an");

    let admin: User = sqlx::query_as(
        "INSERT INTO users (name, email, role, password_hash) VALUES ('Administrator', $1, 'admin', $2) RETURNING *",
    )
    .bind(env_or("SEED_ADMIN_EMAIL", "admin@example.com"))
    .bind(auth::hash_password(&env_or("SEED_ADMIN_PASSWORD", "admin123"))?)
    .fetch_one(db)
    .await?;

    let agent: User = sqlx::query_as(
        "INSERT INTO users (name, email, role, password_hash) VALUES ('Anna Agentin', $1, 'agent', $2) RETURNING *",
    )
    .bind(env_or("SEED_AGENT_EMAIL", "agent@example.com"))
    .bind(auth::hash_password(&env_or("SEED_AGENT_PASSWORD", "agent123"))?)
    .fetch_one(db)
    .await?;

    let support = service::create_group(db, "1st Level Support", "Erste Anlaufstelle für alle Anrufe").await?;
    let billing = service::create_group(db, "Abrechnung", "Rechnungen, Zahlungen, Verträge").await?;
    service::set_group_memberships(db, admin.id, &[support.id, billing.id]).await?;
    service::set_group_memberships(db, agent.id, &[support.id]).await?;

    service::create_org(
        db,
        service::OrgInput {
            name: Some("Acme GmbH".into()),
            domain_names: Some(vec!["acme.example".into()]),
            notes: Some("Premium-Kunde, Rahmenvertrag 2026".into()),
        },
    )
    .await?;
    service::create_org(
        db,
        service::OrgInput {
            name: Some("Beta Logistik AG".into()),
            domain_names: Some(vec!["beta-logistik.example".into()]),
            notes: Some("Standardvertrag, Ansprechpartner IT: Jonas Weber".into()),
        },
    )
    .await?;

    // Kunden; die Organisation ergibt sich aus der E-Mail-Domain.
    let mut customers = Vec::new();
    let portal_password = env_or("SEED_CUSTOMER_PASSWORD", "kunde1234");
    for (i, (name, email, phone)) in [
        ("Max Mustermann", Some("max@acme.example"), Some("030 1234567")),
        ("Sabine Koch", Some("sabine.koch@acme.example"), Some("030 1234589")),
        ("Erika Beispiel", Some("erika@example.org"), Some("089 7654321")),
        ("Jonas Weber", Some("j.weber@beta-logistik.example"), Some("040 5551234")),
        ("Thomas Schulz", None, Some("0171 2345678")),
    ]
    .into_iter()
    .enumerate()
    {
        let user = service::create_user(
            db,
            &admin,
            UserInput {
                name: Some(name.into()),
                email: email.map(Into::into),
                phone: phone.map(Into::into),
                // Max Mustermann bekommt einen Portalzugang für die Demo.
                password: (i == 0).then(|| portal_password.clone()),
                ..Default::default()
            },
        )
        .await?;
        customers.push(user);
    }

    let unsolved = json!({ "field": "status", "operator": "less_than", "value": "solved" });
    let views = [
        ("Deine offenen Tickets", json!({ "all": [{ "field": "assignee_id", "operator": "is", "value": "current_user" }, unsolved] }), "updated_at"),
        ("Nicht zugewiesene Tickets", json!({ "all": [{ "field": "assignee_id", "operator": "not_present" }, unsolved] }), "created_at"),
        ("Alle offenen Tickets", json!({ "all": [unsolved] }), "updated_at"),
        ("Telefon-Tickets", json!({ "all": [{ "field": "channel", "operator": "is", "value": "phone" }, unsolved] }), "created_at"),
        ("Dringend & Hoch", json!({ "all": [unsolved, { "field": "priority", "operator": "greater_than", "value": "normal" }] }), "priority"),
        ("Wartend", json!({ "all": [{ "field": "status", "operator": "is", "value": "pending" }] }), "updated_at"),
        ("Kürzlich gelöst", json!({ "all": [{ "field": "status", "operator": "is", "value": "solved" }] }), "updated_at"),
    ];
    for (pos, (title, conditions, sort_by)) in views.into_iter().enumerate() {
        sqlx::query("INSERT INTO views (title, position, conditions, execution) VALUES ($1, $2, $3, $4)")
            .bind(title)
            .bind(pos as i32)
            .bind(conditions)
            .bind(json!({ "sort_by": sort_by, "sort_order": "desc" }))
            .execute(db)
            .await?;
    }

    let macros = [
        ("Übernehmen", "Ticket mir zuweisen und öffnen", json!([
            { "field": "assignee_id", "value": "current_user" },
            { "field": "status", "value": "open" }
        ])),
        ("Rückruf vereinbart", "Rückruf-Tag setzen, auf Wartend stellen", json!([
            { "field": "add_tags", "value": "rueckruf" },
            { "field": "status", "value": "pending" },
            { "field": "comment_private", "value": "Rückruf mit dem Kunden vereinbart ({{current_user.name}})." }
        ])),
        ("Kunde antwortet nicht", "Erinnerung senden, Wartend", json!([
            { "field": "status", "value": "pending" },
            { "field": "comment_value", "value": "Guten Tag,\n\nwir warten noch auf Ihre Rückmeldung zu Ticket #{{ticket.id}} „{{ticket.subject}}“.\n\nViele Grüße\n{{current_user.name}}" }
        ])),
        ("Am Telefon gelöst", "Lösen mit Abschlussnachricht", json!([
            { "field": "status", "value": "solved" },
            { "field": "add_tags", "value": "first_call_resolution" },
            { "field": "comment_value", "value": "Wie telefonisch besprochen ist Ihr Anliegen gelöst. Bei Fragen antworten Sie einfach auf diese Nachricht.\n\n{{current_user.name}}" }
        ])),
        ("An Abrechnung übergeben", "Gruppe Abrechnung, Zuweisung entfernen", json!([
            { "field": "group_id", "value": billing.id },
            { "field": "assignee_id", "value": null },
            { "field": "comment_private", "value": "An die Abrechnung übergeben." }
        ])),
    ];
    for (pos, (title, description, actions)) in macros.into_iter().enumerate() {
        sqlx::query("INSERT INTO macros (title, description, position, actions) VALUES ($1, $2, $3, $4)")
            .bind(title)
            .bind(description)
            .bind(pos as i32)
            .bind(actions)
            .execute(db)
            .await?;
    }

    let triggers = [
        ("Eingangsbestätigung an Anfragenden",
         json!({ "all": [{ "field": "update_type", "operator": "is", "value": "Create" }] }),
         json!([{ "field": "notify_requester", "value": "Ihre Anfrage #{{ticket.id}} „{{ticket.subject}}“ ist bei uns eingegangen." }])),
        ("Telefon-Tickets markieren",
         json!({ "all": [{ "field": "update_type", "operator": "is", "value": "Create" }, { "field": "channel", "operator": "is", "value": "phone" }] }),
         json!([{ "field": "add_tags", "value": "anruf" }])),
        ("Dringende Tickets an 1st Level",
         json!({ "all": [{ "field": "priority", "operator": "is", "value": "urgent" }, { "field": "group_id", "operator": "not_present" }] }),
         json!([{ "field": "group_id", "value": support.id }])),
        ("Neue Tickets ohne Gruppe an 1st Level",
         json!({ "all": [{ "field": "update_type", "operator": "is", "value": "Create" }, { "field": "group_id", "operator": "not_present" }] }),
         json!([{ "field": "group_id", "value": support.id }])),
        ("Lösungsbenachrichtigung",
         json!({ "all": [{ "field": "status", "operator": "changed_to", "value": "solved" }] }),
         json!([{ "field": "notify_requester", "value": "Ihr Ticket #{{ticket.id}} wurde gelöst." }])),
    ];
    for (pos, (title, conditions, actions)) in triggers.into_iter().enumerate() {
        sqlx::query("INSERT INTO triggers (title, position, conditions, actions) VALUES ($1, $2, $3, $4)")
            .bind(title)
            .bind(pos as i32)
            .bind(conditions)
            .bind(actions)
            .execute(db)
            .await?;
    }

    let mut metrics = Vec::new();
    for (prio, reply, resolve) in [("urgent", 30, 240), ("high", 120, 960), ("normal", 480, 2880), ("low", 1440, 7200)] {
        metrics.push(json!({ "priority": prio, "metric": "first_reply_time", "target": reply }));
        metrics.push(json!({ "priority": prio, "metric": "full_resolution_time", "target": resolve }));
    }
    sqlx::query("INSERT INTO sla_policies (title, description, filter, metrics) VALUES ($1, $2, $3, $4)")
        .bind("Standard-SLA")
        .bind("Gilt für alle Tickets")
        .bind(json!({ "all": [], "any": [] }))
        .bind(json!(metrics))
        .execute(db)
        .await?;

    service::create_field(
        db,
        service::FieldInput {
            key: "produkt".into(),
            label: "Produkt".into(),
            field_type: "dropdown".into(),
            options: ["Internet", "Telefonie", "Hardware", "Abrechnung"].map(String::from).to_vec(),
            active: true,
            ..Default::default()
        },
    )
    .await?;
    service::create_field(
        db,
        service::FieldInput {
            key: "vertragsnummer".into(),
            label: "Vertragsnummer".into(),
            field_type: "text".into(),
            position: 1,
            active: true,
            ..Default::default()
        },
    )
    .await?;

    demo_articles(db, agent.id).await?;
    demo_tickets(db, &admin, &agent, &customers).await?;
    // Ab jetzt verteilt der 1st Level neue Tickets automatisch an Agenten, die online sind.
    service::set_group_assignment(db, support.id, "least_loaded").await?;
    Ok(())
}

/// Hilfe-Artikel für das Hilfe-Center (passend zu den Demo-Tickets).
async fn demo_articles(db: &PgPool, author_id: i64) -> anyhow::Result<()> {
    use crate::kb::{self, ArticleInput};
    kb::create_category(db, "Internet & Telefonie").await?;
    kb::create_category(db, "Konto & Abrechnung").await?;
    let cats: Vec<(i64, String)> = kb::categories(db).await?;
    let cat = |name: &str| cats.iter().find(|(_, n)| n == name).map(|(id, _)| *id);
    let articles = [
        ("Internet & Telefonie", "Kein Internet? Router neu starten",
         "Wenn das Internet ausgefallen ist, hilft in den meisten Fällen ein Neustart des Routers:\n\n1. Stecker des Routers ziehen.\n2. 30 Sekunden warten.\n3. Stecker wieder einstecken und 3 Minuten warten, bis die Internet-Leuchte dauerhaft grün ist.\n\nBlinkt die Leuchte weiterhin rot, liegt vermutlich eine Störung vor. Prüfen Sie dann unsere Störungsseite oder rufen Sie uns an.", "published"),
        ("Internet & Telefonie", "Langsame Verbindung oder VPN-Probleme",
         "Ist die Verbindung langsam, testen Sie zuerst per LAN-Kabel statt WLAN. Bei VPN-Verbindungen kann der Anbieter des VPN-Servers die Ursache sein.\n\nNotieren Sie Uhrzeit und Ergebnis eines Speedtests – das hilft uns bei der Analyse.", "published"),
        ("Konto & Abrechnung", "Rechnung verstehen und doppelte Abbuchung melden",
         "Ihre Rechnung finden Sie monatlich im Kundenportal. Wurde ein Betrag doppelt abgebucht, stellen Sie bitte eine Anfrage mit Rechnungsnummer und Abbuchungsdatum. Die Erstattung erfolgt innerhalb von 5 Werktagen.", "published"),
        ("Konto & Abrechnung", "Tarifwechsel: so funktioniert's",
         "Ein Tarifwechsel ist jeweils zum Monatsersten möglich. Der Business-Tarif kostet 39,90 € im Monat. Stellen Sie einfach eine Anfrage mit dem gewünschten Tarif – wir bestätigen den Wechsel per E-Mail.", "published"),
        ("Konto & Abrechnung", "Passwort für das Kundenportal zurücksetzen",
         "Klicken Sie auf der Anmeldeseite des Kundenportals auf „Passwort vergessen?“, geben Sie Ihre E-Mail-Adresse ein und folgen Sie dem Link in der E-Mail. Der Link ist eine Stunde gültig.", "published"),
        ("Internet & Telefonie", "Intern: Eskalation bei Großstörungen",
         "Bei mehr als 10 Störungsmeldungen in 30 Minuten: Teamleitung informieren, Störungsseite aktualisieren, Makro „Störung bekannt“ verwenden.", "draft"),
    ];
    for (category, title, body, status) in articles {
        kb::save(
            db,
            None,
            ArticleInput { title: title.into(), body: body.into(), category_id: cat(category), status: status.into() },
            author_id,
        )
        .await?;
    }
    Ok(())
}

/// Demo-Tickets mit realistischem Verlauf: alle Status, Antworten, interne Notizen,
/// Makros, Kundenantworten und eine verletzte SLA.
async fn demo_tickets(db: &PgPool, admin: &User, anna: &User, c: &[User]) -> anyhow::Result<()> {
    let (max, sabine, erika, jonas, thomas) = (&c[0], &c[1], &c[2], &c[3], &c[4]);

    // 1: Störung, Anna kümmert sich
    let t = ticket(db, anna, max, "Internet im Büro ausgefallen", "Kunde ruft an: Seit heute Morgen kein Internet an allen Arbeitsplätzen, Router blinkt rot.", "phone", "urgent", "incident").await?;
    reply(db, anna, t, "Guten Tag Herr Mustermann,\n\nwir haben eine Störung in Ihrem Anschlussbereich festgestellt. Ein Techniker ist informiert, wir melden uns spätestens in 2 Stunden.\n\nViele Grüße\nAnna Agentin", true, None, Some(anna.id)).await?;
    shift(db, t, 2).await?;
    durations(db, t, 22, None).await?;

    // 2: Rechnung, an die Abrechnung übergeben, noch keine Antwort → SLA verletzt
    let t = ticket(db, anna, max, "Rechnung März doppelt abgebucht", "Hallo,\n\nder Rechnungsbetrag für März wurde zweimal von unserem Konto eingezogen. Bitte erstatten Sie den doppelten Betrag.\n\nMax Mustermann", "email", "high", "problem").await?;
    apply_macro(db, admin, t, "An Abrechnung übergeben").await?;
    shift(db, t, 5).await?;

    // 3: Frage beantwortet, wartet auf den Kunden
    let t = ticket(db, anna, erika, "Frage zum Tarifwechsel", "Kann ich zum nächsten Monat in den Business-Tarif wechseln? Was kostet das?", "web", "normal", "question").await?;
    reply(db, anna, t, "Hallo Frau Beispiel,\n\nja, ein Wechsel ist zum Monatsersten möglich. Der Business-Tarif kostet 39,90 € im Monat. Soll ich den Wechsel für Sie beauftragen?\n\nViele Grüße\nAnna Agentin", true, Some("pending"), Some(anna.id)).await?;
    shift(db, t, 26).await?;
    durations(db, t, 95, None).await?;

    // 4: Am Telefon gelöst
    let t = ticket(db, anna, thomas, "Passwort für Kundenportal zurücksetzen", "Anrufer kommt nicht mehr ins Kundenportal, Passwort vergessen. Identität über Kundennummer geprüft.", "phone", "normal", "question").await?;
    reply(db, anna, t, "Rücksetz-Link per SMS verschickt, Kunde konnte sich wieder anmelden.", false, None, Some(anna.id)).await?;
    apply_macro(db, anna, t, "Am Telefon gelöst").await?;
    shift(db, t, 30).await?;
    durations(db, t, 9, Some(12)).await?;

    // 5: Ganz frisch, noch unbearbeitet
    let t = ticket(db, anna, sabine, "Lieferung beschädigt angekommen", "Hallo,\n\ndie heute gelieferten Geräte (Bestellung 4711) sind beschädigt, zwei Displays haben Risse. Fotos anbei.\n\nSabine Koch", "email", "high", "incident").await?;
    shift_minutes(db, t, 40).await?;

    // 6: Angehalten, wartet auf Dritte
    let t = ticket(db, admin, jonas, "VPN-Verbindung sehr langsam", "Anruf von Herrn Weber: VPN-Verbindung seit gestern extrem langsam, betrifft alle Außendienstler.", "phone", "normal", "incident").await?;
    reply(db, admin, t, "Problem liegt beim Upstream-Provider, Ticket dort eröffnet (Ref. UP-88231). Angehalten bis Rückmeldung.", false, Some("hold"), Some(admin.id)).await?;
    shift(db, t, 20).await?;

    // 7: Neu, niedrige Priorität
    let t = ticket(db, anna, jonas, "Kündigung Zusatzoption zum Quartalsende", "Bitte kündigen Sie die Zusatzoption \"Backup Plus\" zum Ende des Quartals.\n\nJonas Weber\nBeta Logistik AG", "email", "low", "task").await?;
    shift(db, t, 3).await?;

    // 8: Rückruf vereinbart
    let t = ticket(db, anna, sabine, "Rückruf wegen Angebot gewünscht", "Frau Koch möchte ein Angebot für 20 weitere Arbeitsplätze besprechen, bittet um Rückruf morgen Vormittag.", "phone", "normal", "question").await?;
    apply_macro(db, anna, t, "Übernehmen").await?;
    apply_macro(db, anna, t, "Rückruf vereinbart").await?;
    shift(db, t, 6).await?;

    // 9: Kunde antwortet, Ticket wird wieder geöffnet und dann gelöst
    let t = ticket(db, anna, erika, "E-Mail-Postfach voll", "Ich kann keine E-Mails mehr empfangen, angeblich ist mein Postfach voll.", "web", "low", "question").await?;
    reply(db, anna, t, "Hallo Frau Beispiel,\n\nich habe Ihr Postfach auf 10 GB erweitert. Bitte prüfen Sie, ob wieder E-Mails ankommen.\n\nViele Grüße\nAnna Agentin", true, Some("pending"), Some(anna.id)).await?;
    customer_reply(db, anna, t, erika, "Danke, es funktioniert wieder!").await?;
    reply(db, anna, t, "Prima, dann schließe ich die Anfrage. Viele Grüße!", true, Some("solved"), None).await?;
    shift(db, t, 50).await?;
    durations(db, t, 48, Some(1430)).await?;

    // 10: Kunde hat gerade geantwortet
    let t = ticket(db, anna, max, "Zugangsdaten für neuen Mitarbeiter", "Wir brauchen einen Zugang für unseren neuen Kollegen Herrn Lehmann ab dem 1. des Monats.", "chat", "normal", "task").await?;
    reply(db, anna, t, "Gerne! Bitte nennen Sie mir noch die gewünschte E-Mail-Adresse von Herrn Lehmann.", true, Some("pending"), Some(anna.id)).await?;
    customer_reply(db, anna, t, max, "Die Adresse ist p.lehmann@acme.example").await?;
    shift(db, t, 1).await?;
    durations(db, t, 14, None).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ticket(
    db: &PgPool,
    actor: &User,
    requester: &User,
    subject: &str,
    description: &str,
    channel: &str,
    priority: &str,
    ticket_type: &str,
) -> anyhow::Result<i64> {
    let t = service::create_ticket(
        db,
        actor,
        NewTicket {
            subject: subject.into(),
            description: description.into(),
            // Telefon-Gesprächsnotizen sind intern, Kundennachrichten öffentlich.
            public: channel != "phone",
            requester_id: requester.id,
            channel: channel.into(),
            priority: Some(priority.into()),
            ticket_type: Some(ticket_type.into()),
            custom_fields: json!({}),
            ..Default::default()
        },
        "seed",
    )
    .await?;
    Ok(t.id)
}

async fn reply(
    db: &PgPool,
    actor: &User,
    id: i64,
    body: &str,
    public: bool,
    status: Option<&str>,
    assignee: Option<i64>,
) -> anyhow::Result<()> {
    let changes = TicketChanges {
        status: status.map(Into::into),
        assignee_id: assignee.map(Some),
        comment: Some(CommentChange { body: body.into(), public, author_id: None }),
        ..Default::default()
    };
    service::update_ticket(db, actor, id, changes, None, "seed").await?;
    Ok(())
}

async fn customer_reply(db: &PgPool, actor: &User, id: i64, customer: &User, body: &str) -> anyhow::Result<()> {
    let changes = TicketChanges {
        comment: Some(CommentChange { body: body.into(), public: true, author_id: Some(customer.id) }),
        ..Default::default()
    };
    service::update_ticket(db, actor, id, changes, None, "email").await?;
    Ok(())
}

async fn apply_macro(db: &PgPool, actor: &User, id: i64, title: &str) -> anyhow::Result<()> {
    let macro_id: i64 = sqlx::query_scalar("SELECT id FROM macros WHERE title = $1")
        .bind(title)
        .fetch_one(db)
        .await?;
    service::update_ticket(db, actor, id, TicketChanges::default(), Some(macro_id), "seed").await?;
    Ok(())
}

/// Setzt realistische Antwort-/Lösungszeiten relativ zur Erstellung (für Reports und SLA).
async fn durations(db: &PgPool, id: i64, reply_after_min: i64, solve_after_min: Option<i64>) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE tickets SET
            first_reply_at = CASE WHEN first_reply_at IS NULL THEN NULL ELSE created_at + make_interval(mins => $2) END,
            solved_at = CASE WHEN solved_at IS NULL THEN NULL ELSE created_at + make_interval(mins => $3) END
         WHERE id = $1",
    )
    .bind(id)
    .bind(reply_after_min as i32)
    .bind(solve_after_min.unwrap_or(0) as i32)
    .execute(db)
    .await?;
    Ok(())
}

/// Verschiebt den Verlauf eines Tickets in die Vergangenheit, damit die Demo
/// unterschiedliche Alter und SLA-Zustände zeigt.
async fn shift(db: &PgPool, id: i64, hours: i64) -> anyhow::Result<()> {
    shift_minutes(db, id, hours * 60).await
}

async fn shift_minutes(db: &PgPool, id: i64, minutes: i64) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "UPDATE tickets SET created_at = created_at - make_interval(mins => $2),
            updated_at = updated_at - make_interval(mins => $2),
            first_reply_at = first_reply_at - make_interval(mins => $2),
            solved_at = solved_at - make_interval(mins => $2)
         WHERE id = $1",
    )
    .bind(id)
    .bind(minutes as i32)
    .execute(&mut *tx)
    .await?;
    for table in ["ticket_comments", "ticket_audits"] {
        sqlx::query(&format!(
            "UPDATE {table} SET created_at = created_at - make_interval(mins => $2) WHERE ticket_id = $1"
        ))
        .bind(id)
        .bind(minutes as i32)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
