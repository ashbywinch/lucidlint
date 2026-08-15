def make_item(block):
    return {"kind": "tool_call", "call_id": block.get("id")}
