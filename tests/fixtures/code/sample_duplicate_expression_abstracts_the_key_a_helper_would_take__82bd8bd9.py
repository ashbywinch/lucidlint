import json

def b(store):
    return json.dumps([json.loads(store.get("two").read_text(encoding="utf-8"))], indent=1, ensure_ascii=False) if store and store.exists() else {}
