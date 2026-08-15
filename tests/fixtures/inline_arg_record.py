def render(block):
    return format_({"kind": "tool_call", "call_id": block.get("id")})
