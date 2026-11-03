-- What the firm's documents say about the business: entities (people, organisations, projects...),
-- facts, relationships, and a profile of what the company does. Every item keeps the evidence it
-- came from, and a person only ever sees an item if they may open at least one document that
-- supports it. Nothing here is written unless a firm administrator has switched analysis on.

ALTER TABLE firm_settings ADD COLUMN company_intelligence boolean NOT NULL DEFAULT false;
GRANT UPDATE (company_intelligence) ON firm_settings TO app_api;
-- The worker finds out which firms have analysis on.
GRANT SELECT ON firm_settings TO app_worker;
CREATE POLICY worker_read ON firm_settings FOR SELECT TO app_worker USING (tenant_id = app_tenant());

-- One row per analysed document: what kind of document it is, and the version that was read.
CREATE TABLE document_analysis (
  tenant_id        uuid NOT NULL,
  document_id      uuid NOT NULL,
  content_hash     text NOT NULL,                 -- the document version that was analysed
  analyzer_version integer NOT NULL,
  status           text NOT NULL DEFAULT 'done' CHECK (status IN ('done', 'failed')),
  attempts         integer NOT NULL DEFAULT 0,
  doc_type         text NOT NULL DEFAULT 'other',
  summary          text NOT NULL DEFAULT '',
  model            text NOT NULL DEFAULT '',
  analyzed_at      timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, document_id),
  FOREIGN KEY (tenant_id, document_id) REFERENCES documents (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE entities (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id       uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  kind            text NOT NULL CHECK (kind IN ('person', 'organization', 'project', 'product', 'location', 'other')),
  name            text NOT NULL,
  normalized_name text NOT NULL,
  roles           text[] NOT NULL DEFAULT '{}',   -- customer, vendor, employee, partner, contractor
  status          text NOT NULL DEFAULT 'suggested' CHECK (status IN ('suggested', 'verified', 'rejected')),
  verified_by     text,
  verified_at     timestamptz,
  first_seen_at   timestamptz NOT NULL DEFAULT now(),
  last_seen_at    timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, kind, normalized_name)
);

CREATE TABLE entity_evidence (
  tenant_id   uuid NOT NULL,
  entity_id   uuid NOT NULL,
  document_id uuid NOT NULL,
  quote       text NOT NULL,
  PRIMARY KEY (tenant_id, entity_id, document_id),
  FOREIGN KEY (tenant_id, entity_id) REFERENCES entities (tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, document_id) REFERENCES documents (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE facts (
  id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id         uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  kind              text NOT NULL CHECK (kind IN ('fact', 'procedure', 'decision', 'policy')),
  statement         text NOT NULL,
  normalized        text NOT NULL,
  subject_entity_id uuid,
  status            text NOT NULL DEFAULT 'suggested' CHECK (status IN ('suggested', 'verified', 'rejected', 'outdated')),
  -- A verified fact whose source changed or went away: someone should look at it again.
  needs_review      boolean NOT NULL DEFAULT false,
  verified_by       text,
  verified_at       timestamptz,
  first_seen_at     timestamptz NOT NULL DEFAULT now(),
  last_seen_at      timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, kind, normalized),
  FOREIGN KEY (tenant_id, subject_entity_id) REFERENCES entities (tenant_id, id) ON DELETE SET NULL (subject_entity_id)
);

CREATE TABLE fact_evidence (
  tenant_id    uuid NOT NULL,
  fact_id      uuid NOT NULL,
  document_id  uuid NOT NULL,
  quote        text NOT NULL,
  content_hash text NOT NULL,                      -- the document version the quote came from
  PRIMARY KEY (tenant_id, fact_id, document_id),
  FOREIGN KEY (tenant_id, fact_id) REFERENCES facts (tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, document_id) REFERENCES documents (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE relationships (
  id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id         uuid NOT NULL REFERENCES tenants (id) ON DELETE CASCADE,
  subject_entity_id uuid NOT NULL,
  predicate         text NOT NULL CHECK (predicate IN
    ('works_for', 'customer_of', 'vendor_of', 'manages', 'part_of', 'owns', 'located_in', 'related_to')),
  object_entity_id  uuid NOT NULL,
  status            text NOT NULL DEFAULT 'suggested' CHECK (status IN ('suggested', 'verified', 'rejected')),
  verified_by       text,
  verified_at       timestamptz,
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, subject_entity_id, predicate, object_entity_id),
  FOREIGN KEY (tenant_id, subject_entity_id) REFERENCES entities (tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, object_entity_id) REFERENCES entities (tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE relationship_evidence (
  tenant_id       uuid NOT NULL,
  relationship_id uuid NOT NULL,
  document_id     uuid NOT NULL,
  quote           text NOT NULL,
  PRIMARY KEY (tenant_id, relationship_id, document_id),
  FOREIGN KEY (tenant_id, relationship_id) REFERENCES relationships (tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, document_id) REFERENCES documents (tenant_id, id) ON DELETE CASCADE
);

-- What the company does, in the AI's words, until an administrator confirms or corrects it.
CREATE TABLE company_profile (
  tenant_id       uuid PRIMARY KEY REFERENCES tenants (id) ON DELETE CASCADE,
  summary         text NOT NULL DEFAULT '',
  services        text[] NOT NULL DEFAULT '{}',
  terminology     jsonb NOT NULL DEFAULT '[]',    -- [{"term": "...", "meaning": "..."}]
  status          text NOT NULL DEFAULT 'suggested' CHECK (status IN ('suggested', 'verified')),
  based_on        integer NOT NULL DEFAULT 0,     -- how many analysed documents it was drawn from
  model           text NOT NULL DEFAULT '',
  generated_at    timestamptz NOT NULL DEFAULT now(),
  verified_by     text,
  verified_at     timestamptz
);

DO $$
DECLARE t text;
BEGIN
  FOREACH t IN ARRAY ARRAY['document_analysis', 'entities', 'entity_evidence', 'facts', 'fact_evidence',
                           'relationships', 'relationship_evidence', 'company_profile']
  LOOP
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON %I FROM PUBLIC', t);
    EXECUTE format($p$CREATE POLICY tenant_isolation ON %I FOR ALL TO app_api, app_worker
                      USING (tenant_id = app_tenant()) WITH CHECK (tenant_id = app_tenant())$p$, t);
    EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO app_worker', t);
    EXECUTE format('GRANT SELECT ON %I TO app_api', t);
  END LOOP;
END $$;

-- The API reviews what the worker found: it may confirm, reject or correct, and nothing else.
GRANT UPDATE (name, normalized_name, status, verified_by, verified_at) ON entities TO app_api;
GRANT UPDATE (statement, normalized, status, needs_review, verified_by, verified_at) ON facts TO app_api;
GRANT UPDATE (status, verified_by, verified_at) ON relationships TO app_api;
GRANT UPDATE (summary, services, terminology, status, verified_by, verified_at) ON company_profile TO app_api;

CREATE INDEX entity_evidence_by_document ON entity_evidence (tenant_id, document_id);
CREATE INDEX fact_evidence_by_document ON fact_evidence (tenant_id, document_id);
CREATE INDEX relationship_evidence_by_document ON relationship_evidence (tenant_id, document_id);
