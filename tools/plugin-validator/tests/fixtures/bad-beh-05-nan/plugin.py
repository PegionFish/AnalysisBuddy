import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from _common import Plugin as BasePlugin

class Plugin(BasePlugin):
    plugin_id = "bad-beh-05-nan"

    def on_parse(self, req_id, params):
        sys.stdout.buffer.write(('{"jsonrpc":"2.0","method":"RecordBatch","params":{"file_id":"%s","seq":0,"records":[{"timestamp":1,"metric":"fps","value":NaN}],"done":true}}\n' % params["file_id"]).encode("utf-8"))
        sys.stdout.buffer.flush()
        self.reply(req_id, {"records_total": 1})

if __name__ == "__main__":
    Plugin().run()
