//! Example 08: Event Log Pattern
//!
//! This example demonstrates using TaskStore for event logging/sourcing:
//! - Append-only event log
//! - Event types with payloads
//! - Querying events by type, time range, entity
//!
//! Run with: cargo run --example 08_event_log

use eyre::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use taskstore::{Filter, FilterOp, IndexValue, Record, Store, now_ms};

// ============================================================================
// Event Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
enum EventPayload {
    UserCreated { username: String, email: String },
    UserLoggedIn { ip_address: String },
    UserLoggedOut,
    OrderPlaced { order_id: String, total: i64 },
    OrderShipped { order_id: String, tracking: String },
    PaymentReceived { amount: i64, currency: String },
    SystemAlert { level: String, message: String },
}

impl EventPayload {
    fn event_type(&self) -> &'static str {
        match self {
            EventPayload::UserCreated { .. } => "user_created",
            EventPayload::UserLoggedIn { .. } => "user_logged_in",
            EventPayload::UserLoggedOut => "user_logged_out",
            EventPayload::OrderPlaced { .. } => "order_placed",
            EventPayload::OrderShipped { .. } => "order_shipped",
            EventPayload::PaymentReceived { .. } => "payment_received",
            EventPayload::SystemAlert { .. } => "system_alert",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Event {
    id: String,
    entity_id: String,   // User ID, Order ID, etc.
    entity_type: String, // "user", "order", "system"
    payload: EventPayload,
    timestamp: i64,
    updated_at: i64, // Same as timestamp for events
}

impl Record for Event {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> i64 {
        self.updated_at
    }
    fn collection_name() -> &'static str {
        "events"
    }

    fn indexed_fields(&self) -> HashMap<String, IndexValue> {
        let mut fields = HashMap::new();
        fields.insert(
            "event_type".to_string(),
            IndexValue::String(self.payload.event_type().to_string()),
        );
        fields.insert("entity_id".to_string(), IndexValue::String(self.entity_id.clone()));
        fields.insert("entity_type".to_string(), IndexValue::String(self.entity_type.clone()));
        fields.insert("timestamp".to_string(), IndexValue::Int(self.timestamp));
        fields
    }
}

// ============================================================================
// Event Builder
// ============================================================================

struct EventBuilder {
    counter: u64,
    base_time: i64,
}

impl EventBuilder {
    fn new() -> Self {
        Self {
            counter: 0,
            base_time: now_ms(),
        }
    }

    fn create(&mut self, entity_type: &str, entity_id: &str, payload: EventPayload) -> Event {
        self.counter += 1;
        let timestamp = self.base_time + (self.counter as i64 * 1000); // 1 second apart

        Event {
            id: format!("evt-{:05}", self.counter),
            entity_id: entity_id.to_string(),
            entity_type: entity_type.to_string(),
            payload,
            timestamp,
            updated_at: timestamp,
        }
    }
}

// ============================================================================
// Query Helpers
// ============================================================================

fn get_events_for_entity(store: &Store, entity_id: &str) -> Result<Vec<Event>> {
    store.list(&[Filter {
        field: "entity_id".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String(entity_id.to_string()),
    }])
}

fn get_events_by_type(store: &Store, event_type: &str) -> Result<Vec<Event>> {
    store.list(&[Filter {
        field: "event_type".to_string(),
        op: FilterOp::Eq,
        value: IndexValue::String(event_type.to_string()),
    }])
}

fn main() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().to_path_buf();

    println!("TaskStore Event Log Example");
    println!("============================\n");

    let mut store = Store::open(&store_path)?;
    let mut builder = EventBuilder::new();

    // Simulate a series of events
    println!("1. Recording events...\n");

    // User registration
    let event = builder.create(
        "user",
        "user-001",
        EventPayload::UserCreated {
            username: "alice".to_string(),
            email: "alice@example.com".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | user-001 | UserCreated", event.id, event.timestamp);

    // User login
    let event = builder.create(
        "user",
        "user-001",
        EventPayload::UserLoggedIn {
            ip_address: "192.168.1.100".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | user-001 | UserLoggedIn", event.id, event.timestamp);

    // Order placed
    let event = builder.create(
        "order",
        "order-001",
        EventPayload::OrderPlaced {
            order_id: "order-001".to_string(),
            total: 9999,
        },
    );
    store.create(event.clone())?;
    println!(
        "   {} | {} | order-001 | OrderPlaced ($99.99)",
        event.id, event.timestamp
    );

    // Payment received
    let event = builder.create(
        "order",
        "order-001",
        EventPayload::PaymentReceived {
            amount: 9999,
            currency: "USD".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | order-001 | PaymentReceived", event.id, event.timestamp);

    // Order shipped
    let event = builder.create(
        "order",
        "order-001",
        EventPayload::OrderShipped {
            order_id: "order-001".to_string(),
            tracking: "1Z999AA10123456784".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | order-001 | OrderShipped", event.id, event.timestamp);

    // Another user
    let event = builder.create(
        "user",
        "user-002",
        EventPayload::UserCreated {
            username: "bob".to_string(),
            email: "bob@example.com".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | user-002 | UserCreated", event.id, event.timestamp);

    // System alert
    let event = builder.create(
        "system",
        "server-01",
        EventPayload::SystemAlert {
            level: "warning".to_string(),
            message: "High CPU usage detected".to_string(),
        },
    );
    store.create(event.clone())?;
    println!("   {} | {} | server-01 | SystemAlert", event.id, event.timestamp);

    // User logout
    let event = builder.create("user", "user-001", EventPayload::UserLoggedOut);
    store.create(event.clone())?;
    println!("   {} | {} | user-001 | UserLoggedOut", event.id, event.timestamp);

    println!();

    // Query: All events for user-001
    println!("2. Events for user-001:");
    let user_events = get_events_for_entity(&store, "user-001")?;
    for event in &user_events {
        println!("   {} | {}", event.payload.event_type(), event.timestamp);
    }
    println!();

    // Query: All events for order-001
    println!("3. Events for order-001:");
    let order_events = get_events_for_entity(&store, "order-001")?;
    for event in &order_events {
        match &event.payload {
            EventPayload::OrderPlaced { total, .. } => {
                println!(
                    "   {} | total=${:.2}",
                    event.payload.event_type(),
                    *total as f64 / 100.0
                );
            }
            EventPayload::OrderShipped { tracking, .. } => {
                println!("   {} | tracking={}", event.payload.event_type(), tracking);
            }
            EventPayload::PaymentReceived { amount, currency } => {
                println!(
                    "   {} | {:.2} {}",
                    event.payload.event_type(),
                    *amount as f64 / 100.0,
                    currency
                );
            }
            _ => {
                println!("   {}", event.payload.event_type());
            }
        }
    }
    println!();

    // Query: All login events
    println!("4. All login events:");
    let logins = get_events_by_type(&store, "user_logged_in")?;
    for event in &logins {
        if let EventPayload::UserLoggedIn { ip_address } = &event.payload {
            println!("   {} logged in from {}", event.entity_id, ip_address);
        }
    }
    println!();

    // Query: System alerts
    println!("5. System alerts:");
    let alerts = get_events_by_type(&store, "system_alert")?;
    for event in &alerts {
        if let EventPayload::SystemAlert { level, message } = &event.payload {
            println!("   [{}] {} - {}", level.to_uppercase(), event.entity_id, message);
        }
    }
    println!();

    // Summary
    println!("6. Event summary:");
    let all_events: Vec<Event> = store.list(&[])?;
    let mut type_counts: HashMap<String, usize> = HashMap::new();
    for event in &all_events {
        *type_counts.entry(event.payload.event_type().to_string()).or_default() += 1;
    }
    println!("   Total events: {}", all_events.len());
    for (event_type, count) in &type_counts {
        println!("   - {}: {}", event_type, count);
    }
    println!();

    println!("Example complete!");
    println!("\nKey points:");
    println!("  - Events are append-only records");
    println!("  - Each event has entity_id for grouping");
    println!("  - Payload uses serde tagged enum for type safety");
    println!("  - Query by entity_id, event_type, or timestamp range");

    Ok(())
}
