def run_tool(tool, args):
    if tool == "a":
        q = args.get("q", "")
        return {"a": q}
    if tool == "b":
        return {"b": q}
    if tool == "c":
        return {"c": args.get("id", "")}
    return {"error": "unknown"}
