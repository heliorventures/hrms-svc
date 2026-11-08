CREATE TABLE "user" (id UUID PRIMARY KEY,tenant_id UUID NOT NULL);
CREATE TABLE location (
    id UUID PRIMARY KEY,tenant_id UUID NOT NULL,name VARCHAR(255) NOT NULL,
    address VARCHAR(500),city VARCHAR(100),state VARCHAR(100),country VARCHAR(100),
    geo_lat NUMERIC(10,7),geo_lng NUMERIC(10,7),geo_fence_radius_meters NUMERIC(12,2),
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,deleted_at TIMESTAMPTZ,deleted_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX uq_location_active_normalized_name
    ON location(tenant_id,lower(regexp_replace(btrim(name),'\s+',' ','g'))) WHERE NOT is_deleted;
-- The actual foundation function body and location update trigger.
CREATE SCHEMA IF NOT EXISTS kabipay_ops;
CREATE OR REPLACE FUNCTION kabipay_ops.set_updated_at() RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER trg_location_updated_at BEFORE UPDATE ON location
    FOR EACH ROW EXECUTE FUNCTION kabipay_ops.set_updated_at();
CREATE TABLE audit_log (
    id UUID PRIMARY KEY,tenant_id UUID NOT NULL,user_id UUID REFERENCES "user"(id) ON DELETE SET NULL,
    entity_type VARCHAR(100) NOT NULL,entity_id UUID,action VARCHAR(100) NOT NULL,
    before_state JSONB,after_state JSONB,ip_address VARCHAR(45),user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
