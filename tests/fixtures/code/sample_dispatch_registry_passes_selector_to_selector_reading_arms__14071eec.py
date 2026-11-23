def run_tool(tool, args):
    if tool == "search":
        q = args.get("q", "")
        return {"tool": tool, "q": q}
    if tool == "person":
        i = args.get("id", "")
        return {"tool": tool, "id": i}
    if tool == "rels":
        ids = args.get("ids", [])
        return {"tool": tool, "n": len(ids)}
    return {"error": "unknown"}
