//! Real adapter + real ccteam relay, with deterministic offline vendor/CLI
//! peers. All environment mutation is in this isolated integration process.
#![cfg(unix)]

use ccteam_harness::execution::claude_accounts::{account_id, ClaudeAccountBinding};
use ccteam_harness::{
    AgentSpecBrief, ClaudeStreamJsonAdapter, HarnessAdapter, PermissionMode, SpawnCtx, ThreadEvent,
    TurnInput,
};
use futures::StreamExt;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

const FAKE_CLAUDE: &str = r#"#!/usr/bin/python3
import json, os, sys
from pathlib import Path
profile = Path(os.environ.get('CLAUDE_CONFIG_DIR', str(Path.home() / '.claude')))
email = os.environ.get('CSWAP_FAKE_DEFAULT_EMAIL', 'two@example.test') if 'CLAUDE_CONFIG_DIR' not in os.environ else 'two@example.test'
if sys.argv[1:3] == ['auth', 'status']:
    print(json.dumps({'loggedIn':True,'authMethod':'claude.ai','email':email,'orgId':''}))
    sys.exit(0)
args = sys.argv[1:]
assert '--no-chrome' in args and '--input-format' in args
log = {'argv':args,'profile':str(profile),'pid':os.getpid(),'isatty':[sys.stdin.isatty(),sys.stdout.isatty(),sys.stderr.isatty()], 'api_key':os.environ.get('ANTHROPIC_API_KEY'), 'path':os.environ['PATH']}
Path(os.environ['CSWAP_FAKE_LOG']).write_text(json.dumps(log))
def emit(obj):
    print(json.dumps(obj), flush=True)
for line in sys.stdin:
    request=json.loads(line)
    if request['type']=='control_request':
        emit({'type':'control_response','response':{'subtype':'success','request_id':request['request_id'],'response':{'commands':[],'models':[{'value':'fake-model'}]}}})
    elif request['type']=='user':
        emit({'type':'assistant','message':{'id':'msg-test','role':'assistant','model':'fake-model','content':[{'type':'text','text':'account reply'}]}})
        emit({'type':'result','subtype':'success','is_error':False,'result':'account reply','usage':{'input_tokens':1,'output_tokens':1}})
"#;

const FAKE_CSWAP: &str = r#"#!/usr/bin/python3
import json, os, shutil, subprocess, sys
from pathlib import Path
log=Path(os.environ['CSWAP_FAKE_CALLS'])
with log.open('a') as output:
    output.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1:]==['list','--json']:
    print(json.dumps({'schemaVersion':1,'accounts':[{'number':2,'email':os.environ.get('CSWAP_FAKE_ACCOUNT_EMAIL','two@example.test'),'organizationUuid':'','alias':'work','active':os.environ.get('CSWAP_FAKE_ACTIVE')=='1'}]}))
    sys.exit(0)
if sys.argv[1:]==['run','--help']:
    print('--no-share-history --require-session')
    sys.exit(0)
assert sys.argv[1:3]==['run','2']
assert '--no-share-history' in sys.argv
active=os.environ.get('CSWAP_FAKE_ACTIVE')=='1'
if active and '--require-session' in sys.argv:
    sys.exit(4)
env=dict(os.environ)
if not active:
    profile=Path.home()/'cswap-profiles'/'two'
    profile.mkdir(parents=True,exist_ok=True)
    env['CLAUDE_CONFIG_DIR']=str(profile)
    for key in ['ANTHROPIC_API_KEY','ANTHROPIC_AUTH_TOKEN','CLAUDE_CODE_OAUTH_TOKEN']:
        env.pop(key,None)
    subprocess.run([shutil.which('claude'),'auth','status','--json'],env=env,check=True,capture_output=True)
print('Launching account two@example.test', flush=True)
args=sys.argv[sys.argv.index('--')+1:]
os.execvpe('claude',['claude',*args],env)
"#;

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn ctx(root: &Path, sid: &str, binding: Option<ClaudeAccountBinding>) -> SpawnCtx {
    SpawnCtx {
        claude_account: binding,
        generation: 1,
        slug: "probe".into(),
        sid: sid.into(),
        owner: "user:web-api".into(),
        cwd: root.into(),
        project_dir: root.into(),
        extra_args: vec![],
        model_id: None,
        effort: None,
        mode: None,
        permission_mode: PermissionMode::Skip,
        secret: String::new(),
        remote: None,
    }
}

fn log(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(root.join("argv.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn account_launches_preserve_protocol_profiles_and_default_optional_behavior() {
    let real_cswap = std::env::var_os("CCTEAM_TEST_REAL_CSWAP_BIN");
    let real_claude = std::env::var_os("CCTEAM_TEST_REAL_CLAUDE_BIN");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::env::set_var("HOME", &home);
    std::env::set_var("CCTEAM_HOME", home.join(".ccteam"));
    std::env::set_var("CLAUDE_CONFIG_HOME", home.join(".claude"));
    std::env::set_var("XDG_DATA_HOME", root.join("data"));
    std::env::set_var("PATH", "/usr/bin:/bin");
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        std::env::remove_var(key);
    }
    std::env::set_var("CCTEAM_CSWAP_RELAY_BIN", env!("CARGO_BIN_EXE_ccteam"));
    std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "5000");
    std::env::set_var("CSWAP_FAKE_LOG", root.join("argv.json"));
    std::env::set_var("CSWAP_FAKE_CALLS", root.join("cswap.jsonl"));
    let claude = root.join("real-claude");
    let cswap = root.join("cswap");
    script(&claude, FAKE_CLAUDE);
    script(&cswap, FAKE_CSWAP);
    std::env::set_var("CCTEAM_CLAUDE_BIN", &claude);
    std::env::set_var("CCTEAM_CSWAP_BIN", root.join("missing-cswap"));
    let spec = AgentSpecBrief {
        role: String::new(),
    };

    // No cswap is installed: the ordinary adapter still starts and resumes.
    let adapter = ClaudeStreamJsonAdapter::new();
    let normal = adapter
        .start_thread(&spec, &ctx(root, "s1", None))
        .await
        .unwrap();
    assert!(!root.join("cswap.jsonl").exists());
    assert_eq!(
        log(root)["profile"],
        home.join(".claude").to_string_lossy().as_ref()
    );
    adapter.close_thread(&normal).await.unwrap();

    std::env::set_var("CCTEAM_CSWAP_BIN", &cswap);
    std::env::set_var("ANTHROPIC_API_KEY", "sandbox-ambient-key-must-not-win");
    let requested = ClaudeAccountBinding::requested(account_id("two@example.test", "")).unwrap();
    let selected = adapter
        .start_thread(&spec, &ctx(root, "s2", Some(requested.clone())))
        .await
        .unwrap();
    let binding: ClaudeAccountBinding =
        serde_json::from_value(selected.raw_extras["claude_account"].clone()).unwrap();
    assert!(!binding.default_login);
    assert_eq!(binding.config_dir, Some(home.join("cswap-profiles/two")));
    assert_eq!(
        log(root)["isatty"],
        serde_json::json!([false, false, false])
    );
    assert!(log(root)["api_key"].is_null());
    assert_eq!(log(root)["path"], "/usr/bin:/bin");
    let body: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join(".ccteam/chat/s2/body.json")).unwrap())
            .unwrap();
    assert_eq!(log(root)["pid"], body["pid"]);
    for _ in 0..2 {
        let mut events = adapter.events(&selected);
        adapter
            .submit_turn(&selected, TurnInput::UserText("hello".into()))
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = events.next().await {
                if matches!(event, ThreadEvent::TurnCompleted { .. }) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap();
        assert!(
            completed,
            "each request must complete on the same stream-json process"
        );
        assert!(adapter.thread_is_live(&selected));
    }
    adapter.close_thread(&selected).await.unwrap();

    let transcript = ccteam_harness::execution::transcript_tail::anthropic_project_dir_in(
        &binding.config_dir.as_ref().unwrap().join("projects"),
        root,
    )
    .join(format!("{}.jsonl", selected.identity));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{}\n").unwrap();
    let resumed = adapter
        .start_thread(&spec, &ctx(root, "s2", Some(binding.clone())))
        .await
        .unwrap();
    assert!(log(root)["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|arg| arg == "--resume"));
    assert_eq!(
        log(root)["profile"],
        binding
            .config_dir
            .as_ref()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    adapter.close_thread(&resumed).await.unwrap();

    // A movable slot is not an account identity. Replacing slot 2 fails closed.
    std::env::set_var("CSWAP_FAKE_ACCOUNT_EMAIL", "replacement@example.test");
    assert!(adapter
        .start_thread(&spec, &ctx(root, "s2", Some(binding.clone())))
        .await
        .is_err());
    std::env::remove_var("CSWAP_FAKE_ACCOUNT_EMAIL");

    // An already isolated sid cannot move into the default profile on resume.
    std::env::set_var("CSWAP_FAKE_ACTIVE", "1");
    assert!(adapter
        .start_thread(&spec, &ctx(root, "s2", Some(binding)))
        .await
        .is_err());
    assert!(transcript.exists());

    // A NEW selection may fall back to the active/default login (owner choice).
    let fallback = adapter
        .start_thread(&spec, &ctx(root, "s3", Some(requested.clone())))
        .await
        .unwrap();
    let default_binding: ClaudeAccountBinding =
        serde_json::from_value(fallback.raw_extras["claude_account"].clone()).unwrap();
    assert!(default_binding.default_login);
    assert_eq!(default_binding.config_dir, Some(home.join(".claude")));
    assert!(log(root)["api_key"].is_null());
    adapter.close_thread(&fallback).await.unwrap();

    // Auto has changed the default identity and cswap has been uninstalled:
    // the fallback sid must still resume in the original default profile.
    std::env::set_var("CSWAP_FAKE_DEFAULT_EMAIL", "auto-switched@example.test");
    std::env::set_var("CCTEAM_CSWAP_BIN", root.join("missing-cswap"));
    let followed = adapter
        .start_thread(&spec, &ctx(root, "s3", Some(default_binding)))
        .await
        .unwrap();
    assert_eq!(
        log(root)["profile"],
        home.join(".claude").to_string_lossy().as_ref()
    );
    adapter.close_thread(&followed).await.unwrap();
    assert!(adapter
        .start_thread(&spec, &ctx(root, "s4", Some(requested)))
        .await
        .is_err());

    // Optional compatibility probe: a fresh sandbox store with a fake token;
    // no real account or credentials are read or copied.
    if let Some(real_cswap) = real_cswap {
        std::env::remove_var("CSWAP_FAKE_ACTIVE");
        std::env::remove_var("ANTHROPIC_API_KEY");
        let guard = root.join("netguard");
        std::fs::create_dir_all(&guard).unwrap();
        std::fs::write(guard.join("sitecustomize.py"), "import socket\ndef deny(*args, **kwargs):\n    raise RuntimeError('network disabled for isolated cswap probe')\nsocket.socket.connect=deny\nsocket.create_connection=deny\n").unwrap();
        std::env::set_var("PYTHONPATH", &guard);
        let result = std::process::Command::new(&real_cswap)
            .args([
                "add-token",
                "sk-ant-oat01-SANDBOX-NOT-REAL",
                "--email",
                "two@example.test",
                "--slot",
                "2",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "sandbox setup failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::env::set_var("CCTEAM_CSWAP_BIN", real_cswap);
        let selection =
            ClaudeAccountBinding::requested(account_id("two@example.test", "")).unwrap();
        let real_launcher = adapter
            .start_thread(&spec, &ctx(root, "s5", Some(selection.clone())))
            .await
            .unwrap();
        assert_eq!(
            real_launcher.raw_extras["claude_account"]["default_login"],
            false
        );
        assert_eq!(
            log(root)["isatty"],
            serde_json::json!([false, false, false])
        );
        adapter.close_thread(&real_launcher).await.unwrap();
        if let Some(real_claude) = real_claude {
            // Real binaries, initialize only: no user turn or model inference.
            std::env::set_var("CCTEAM_CLAUDE_BIN", real_claude);
            std::env::set_var("CCTEAM_STREAM_JSON_INIT_TIMEOUT_MS", "30000");
            std::env::set_var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
            std::env::set_var("DISABLE_TELEMETRY", "1");
            let real = adapter
                .start_thread(&spec, &ctx(root, "s6", Some(selection)))
                .await
                .unwrap();
            assert!(adapter.thread_is_live(&real));
            assert_eq!(real.raw_extras["claude_account"]["default_login"], false);
            let profile = std::path::PathBuf::from(
                real.raw_extras["claude_account"]["config_dir"]
                    .as_str()
                    .unwrap(),
            );
            let body: serde_json::Value = serde_json::from_slice(
                &std::fs::read(root.join(".ccteam/chat/s6/body.json")).unwrap(),
            )
            .unwrap();
            let pid = body["pid"].as_u64().unwrap();
            let pid_record = profile.join("sessions").join(format!("{pid}.json"));
            eprintln!("real cswap + Claude stream-json initialize passed; native session PID record exists: {}", pid_record.exists());
            assert!(
                pid_record.exists(),
                "cswap must be able to discover and protect the headless Claude process"
            );
            adapter.close_thread(&real).await.unwrap();
        }
    }
}
