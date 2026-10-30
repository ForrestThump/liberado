//! [`DeliverySink`] for the outbox worker.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_outbox::{DeliveryBatch, DeliveryOutcome, DeliverySink, Outbox};

use liberado_notify::telegram_send_accepted;
use liberado_notify::web_push::{self, PushDelivery, REMINDER_TTL};

pub struct OutboxDelivery {
    outbox: Arc<Outbox>,
    vapid: Option<web_push::VapidKeys>,
    telegram: Option<Arc<TelegramReminderSender>>,
}

pub struct TelegramReminderSender {
    token: String,
    chat_id: String,
    client: reqwest::Client,
}

impl OutboxDelivery {
    pub fn new(
        outbox: Arc<Outbox>,
        vapid: Option<web_push::VapidKeys>,
        telegram: Option<Arc<TelegramReminderSender>>,
    ) -> Self {
        Self {
            outbox,
            vapid,
            telegram,
        }
    }

    pub fn vapid_public_key(&self) -> Option<String> {
        self.vapid
            .as_ref()
            .map(|keys| keys.public_key().to_string())
    }

    pub fn vapid_configured(&self) -> bool {
        self.vapid.is_some()
    }
}

impl TelegramReminderSender {
    pub fn from_env() -> Option<Arc<Self>> {
        let token = std::env::var("LIBERADO_TELEGRAM_REMINDER_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("LIBERADO_TELEGRAM_REMINDER_CHAT_ID").ok()?;
        if token.trim().is_empty() || chat_id.trim().is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            token,
            chat_id,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .ok()?,
        }))
    }

    pub fn ready(&self) -> bool {
        true
    }

    async fn send(&self, text: &str) -> Result<(), String> {
        let response = self
            .client
            .post(format!(
                "https://api.telegram.org/bot{}/sendMessage",
                self.token
            ))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        if telegram_send_accepted(status, &body) {
            Ok(())
        } else {
            Err(format!("telegram {status}"))
        }
    }
}

#[async_trait]
impl DeliverySink for OutboxDelivery {
    fn readiness(&self, channel: &str, target: &str) -> std::result::Result<(), String> {
        match channel {
            "webpush" => {
                if self.vapid.is_none() {
                    return Err(
                        "set LIBERADO_VAPID_PUBLIC_KEY, LIBERADO_VAPID_PRIVATE_KEY, LIBERADO_VAPID_SUBJECT"
                            .into(),
                    );
                }
                if target.is_empty() || target == "sub:" {
                    return Err("no push subscriptions".into());
                }
                Ok(())
            }
            "telegram" => {
                if self.telegram.is_none() {
                    return Err("reminder Telegram bot is not configured".into());
                }
                Ok(())
            }
            "matrix" => Err("matrix reminders are disabled".into()),
            other => Err(format!("unknown channel '{other}'")),
        }
    }

    async fn deliver(&self, batch: &DeliveryBatch) -> DeliveryOutcome {
        match batch.channel.as_str() {
            "webpush" => self.deliver_webpush(batch).await,
            "telegram" => self.deliver_telegram(batch).await,
            other => DeliveryOutcome::Unconfigured(format!("channel '{other}' is not wired")),
        }
    }
}

impl OutboxDelivery {
    async fn deliver_webpush(&self, batch: &DeliveryBatch) -> DeliveryOutcome {
        let Some(keys) = self.vapid.as_ref() else {
            return DeliveryOutcome::Unconfigured(
                "set LIBERADO_VAPID_PUBLIC_KEY, LIBERADO_VAPID_PRIVATE_KEY, LIBERADO_VAPID_SUBJECT"
                    .into(),
            );
        };
        let Some(sub_id) = batch.target.strip_prefix("sub:") else {
            return DeliveryOutcome::Unconfigured("push target is missing".into());
        };
        let Ok(id) = sub_id.parse::<i64>() else {
            return DeliveryOutcome::Retryable("push target id".into());
        };
        let Ok(Some(sub)) = self.outbox.subscription_keys(id) else {
            return DeliveryOutcome::Gone;
        };
        let rows_json = serde_json::to_string(&batch.rows).unwrap_or_else(|_| "[]".to_string());
        let payload =
            web_push::reminder_payload(&batch.title, &batch.body, &batch.tag, &rows_json, "/");
        let delivery = web_push::post_push(
            keys,
            web_push::PushRequest {
                endpoint: &sub.endpoint,
                p256dh: &sub.p256dh,
                auth: &sub.auth,
                payload: payload.as_bytes(),
                ttl_secs: REMINDER_TTL,
                urgency: "high",
            },
        )
        .await;
        match delivery {
            PushDelivery::Accepted => DeliveryOutcome::Accepted,
            PushDelivery::Gone => DeliveryOutcome::Gone,
            PushDelivery::Failed => DeliveryOutcome::Retryable("web push rejected".into()),
        }
    }

    async fn deliver_telegram(&self, batch: &DeliveryBatch) -> DeliveryOutcome {
        let Some(sender) = self.telegram.as_ref() else {
            return DeliveryOutcome::Unconfigured("reminder Telegram bot is not configured".into());
        };
        let text = format!("{}\n{}", batch.title, batch.body);
        match sender.send(&text).await {
            Ok(()) => DeliveryOutcome::Accepted,
            Err(message) => DeliveryOutcome::Retryable(message),
        }
    }
}
