from pathlib import Path

def a(root):
    return root / Path("page").with_suffix(".layout.json")
