-- Datenmodell angelehnt an Zendesk Support (Tickets, Users, Organizations, Groups,
-- Views, Macros, Triggers, SLA Policies, Ticket Audits).

CREATE TABLE organizations (
    id           BIGSERIAL PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    domain_names TEXT[] NOT NULL DEFAULT '{}',
    notes        TEXT NOT NULL DEFAULT '',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id              BIGSERIAL PRIMARY KEY,
    name            TEXT NOT NULL,
    email           TEXT,
    phone           TEXT,
    role            TEXT NOT NULL DEFAULT 'end-user' CHECK (role IN ('end-user', 'agent', 'admin')),
    organization_id BIGINT REFERENCES organizations(id) ON DELETE SET NULL,
    password_hash   TEXT,
    active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email_key ON users (lower(email));
CREATE INDEX users_phone_idx ON users (phone);
CREATE INDEX users_org_idx ON users (organization_id);

CREATE TABLE agent_groups (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE group_memberships (
    group_id BIGINT NOT NULL REFERENCES agent_groups(id) ON DELETE CASCADE,
    user_id  BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE sla_policies (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    filter      JSONB NOT NULL DEFAULT '{"all": [], "any": []}',
    metrics     JSONB NOT NULL DEFAULT '[]',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE tickets (
    id              BIGSERIAL PRIMARY KEY,
    subject         TEXT NOT NULL,
    description     TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'new'
                    CHECK (status IN ('new', 'open', 'pending', 'hold', 'solved', 'closed')),
    priority        TEXT CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
    ticket_type     TEXT CHECK (ticket_type IN ('question', 'incident', 'problem', 'task')),
    channel         TEXT NOT NULL DEFAULT 'web'
                    CHECK (channel IN ('web', 'email', 'phone', 'api', 'chat')),
    requester_id    BIGINT NOT NULL REFERENCES users(id),
    submitter_id    BIGINT NOT NULL REFERENCES users(id),
    assignee_id     BIGINT REFERENCES users(id) ON DELETE SET NULL,
    group_id        BIGINT REFERENCES agent_groups(id) ON DELETE SET NULL,
    organization_id BIGINT REFERENCES organizations(id) ON DELETE SET NULL,
    tags            TEXT[] NOT NULL DEFAULT '{}',
    custom_fields   JSONB NOT NULL DEFAULT '{}',
    sla_policy_id   BIGINT REFERENCES sla_policies(id) ON DELETE SET NULL,
    due_at          TIMESTAMPTZ,
    first_reply_at  TIMESTAMPTZ,
    solved_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX tickets_status_idx ON tickets (status);
CREATE INDEX tickets_assignee_idx ON tickets (assignee_id);
CREATE INDEX tickets_requester_idx ON tickets (requester_id);
CREATE INDEX tickets_group_idx ON tickets (group_id);
CREATE INDEX tickets_org_idx ON tickets (organization_id);
CREATE INDEX tickets_tags_idx ON tickets USING GIN (tags);

CREATE TABLE ticket_comments (
    id         BIGSERIAL PRIMARY KEY,
    ticket_id  BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    author_id  BIGINT NOT NULL REFERENCES users(id),
    body       TEXT NOT NULL,
    public     BOOLEAN NOT NULL DEFAULT TRUE,
    via        TEXT NOT NULL DEFAULT 'web',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ticket_comments_ticket_idx ON ticket_comments (ticket_id);

CREATE TABLE ticket_audits (
    id         BIGSERIAL PRIMARY KEY,
    ticket_id  BIGINT NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    author_id  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    via        TEXT NOT NULL DEFAULT 'web',
    events     JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ticket_audits_ticket_idx ON ticket_audits (ticket_id);

CREATE TABLE views (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    conditions  JSONB NOT NULL DEFAULT '{"all": [], "any": []}',
    execution   JSONB NOT NULL DEFAULT '{"sort_by": "updated_at", "sort_order": "desc"}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE macros (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    actions     JSONB NOT NULL DEFAULT '[]',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE triggers (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INT NOT NULL DEFAULT 0,
    active      BOOLEAN NOT NULL DEFAULT TRUE,
    conditions  JSONB NOT NULL DEFAULT '{"all": [], "any": []}',
    actions     JSONB NOT NULL DEFAULT '[]',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    token      TEXT PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
