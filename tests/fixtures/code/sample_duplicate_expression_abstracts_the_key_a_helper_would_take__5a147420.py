import json

def a(store):
    return json.dumps([json.loads(store.get("one").read_text(encoding="utf-8"))], indent=1, ensure_ascii=False) if store and store.exists() else {}
