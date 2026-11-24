-- Indexes for the server's periodic scans: pruning old delivery IDs and
-- expired installation mappings by time, and picking the next unclaimed
-- work by priority and availability, which otherwise read every row.
CREATE INDEX server_deliveries_received_at ON server_deliveries(received_at);
CREATE INDEX server_installations_expires_at ON server_installations(expires_at);
CREATE INDEX server_work_next ON server_work(claimed, priority, available_at);
PRAGMA user_version = 18;
