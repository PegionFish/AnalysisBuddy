import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-08-line"

    def on_parse(self, req_id, params):
        # 单行 > 8 MB -> BEH-08（长度先于内容判定）
        sys.stdout.buffer.write(b'{"x": "' + b"a" * 9000000 + b'"}\n')
        sys.stdout.buffer.flush()

if __name__ == "__main__":
    Plugin().run()
