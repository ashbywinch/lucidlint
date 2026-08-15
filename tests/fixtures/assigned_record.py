def make_item(block):
    item = {"kind": "tool_call", "call_id": block.get("id")}
    return item
