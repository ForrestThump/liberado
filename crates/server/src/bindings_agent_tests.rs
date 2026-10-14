use super::*;

fn matrix_key(room: &str, bot: &str) -> BindingKey {
    BindingKey {
        channel: ChannelKind::Matrix,
        peer: room.to_string(),
        bot: bot.to_string(),
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("liberado-bindings-{tag}-{}", Ulid::new()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn load_in(dir: &Path) -> BindingLoad {
    BindingLoad {
        path: dir.join("channel-bindings.json"),
        legacy_path: dir.join("telegram-sticky-session"),
        adopt_key: None,
    }
}

async fn always_valid(_id: Ulid) -> bool {
    true
}

#[tokio::test]
async fn an_inactive_binding_stops_routing_and_stays_stored() {
    let bindings = ChannelBindings::ephemeral();
    let key = matrix_key("!dm:s", "@liberado_coder:matrix.homelab.local");
    let id = Ulid::new();
    bindings.set(&key, Some(id)).await;
    assert_eq!(bindings.get(&key).await, Some(id));
    assert_eq!(bindings.is_active(&key).await, Some(true));

    bindings.set_active(&key, false).await;
    assert_eq!(bindings.get(&key).await, None);
    assert_eq!(bindings.is_active(&key).await, Some(false));
    assert_eq!(bindings.key_for_session(id).await.unwrap().peer, "!dm:s");
    assert_eq!(bindings.channel_of(id).await, None);
    let again = bindings
        .get_or_create::<_, _, &str>(&key, || async { Err("must reuse the reserved row") })
        .await
        .expect("reserved");
    assert_eq!(again, id);
    assert_eq!(bindings.is_active(&key).await, Some(false));
}

#[tokio::test]
async fn a_gone_agent_session_keeps_the_puppet_binding_inactive() -> std::io::Result<()> {
    let dir = temp_dir("puppet-gone");
    let key = matrix_key("!dm:s", "@liberado_coder:matrix.homelab.local");
    let id = Ulid::new();
    ChannelBindings::load(load_in(&dir), always_valid)
        .await
        .set(&key, Some(id))
        .await;

    let reloaded = ChannelBindings::load(load_in(&dir), |_| async { false }).await;
    assert_eq!(reloaded.get(&key).await, None);
    assert_eq!(reloaded.is_active(&key).await, Some(false));
    assert_eq!(reloaded.key_for_session(id).await.unwrap().bot, key.bot);
    let text = std::fs::read_to_string(dir.join("channel-bindings.json"))?;
    assert!(
        text.contains(&id.to_string()),
        "slug row must stay, got {text}"
    );
    assert!(text.contains("\"active\": false"), "got {text}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[tokio::test]
async fn an_inactive_row_survives_a_missing_session() -> std::io::Result<()> {
    let dir = temp_dir("inactive-kept");
    let key = matrix_key("!dm:s", "@liberado_coder:matrix.homelab.local");
    let id = Ulid::new();
    let bindings = ChannelBindings::load(load_in(&dir), always_valid).await;
    bindings.set(&key, Some(id)).await;
    bindings.set_active(&key, false).await;

    let reloaded = ChannelBindings::load(load_in(&dir), |_| async { false }).await;
    assert_eq!(reloaded.is_active(&key).await, Some(false));
    assert_eq!(reloaded.key_for_session(id).await.unwrap().peer, "!dm:s");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[tokio::test]
async fn two_rooms_route_to_two_sessions() {
    let bindings = ChannelBindings::ephemeral();
    let coder = matrix_key("!coder:s", "@liberado_coder:s");
    let research = matrix_key("!research:s", "@liberado_researcher:s");
    let coder_id = Ulid::new();
    let research_id = Ulid::new();
    bindings.set(&coder, Some(coder_id)).await;
    bindings.set(&research, Some(research_id)).await;
    assert_eq!(bindings.get(&coder).await, Some(coder_id));
    assert_eq!(bindings.get(&research).await, Some(research_id));
    bindings.set_active(&coder, false).await;
    assert_eq!(bindings.get(&coder).await, None);
    assert_eq!(bindings.get(&research).await, Some(research_id));
}
