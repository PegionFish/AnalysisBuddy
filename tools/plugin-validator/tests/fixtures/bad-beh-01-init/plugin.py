import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "other-plugin"  # 与 manifest id 不一致 -> BEH-01

if __name__ == "__main__":
    Plugin().run()
