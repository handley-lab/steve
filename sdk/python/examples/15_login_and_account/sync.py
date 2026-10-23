import getpass
import sys
from pathlib import Path

_EXAMPLES_ROOT = Path(__file__).resolve().parents[1]
if str(_EXAMPLES_ROOT) not in sys.path:
    sys.path.insert(0, str(_EXAMPLES_ROOT))

from _bootstrap import ensure_local_sdk_src, runtime_config

ensure_local_sdk_src()

from steve_sdk import Steve

with Steve(config=runtime_config()) as codex:
    codex.login_api_key(getpass.getpass("Provider API key: "))
    account = codex.account()

    print("account.requires_openai_auth:", account.requires_openai_auth)
