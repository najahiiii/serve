"""Yet another file server"""

import os
import urllib
from datetime import datetime

from flask import Flask, abort, jsonify, render_template, request, send_file
from werkzeug.utils import secure_filename

from utils.common import allowed_file, check, cust_send_file, format_size, gzip_response
from utils.config import ALLOWED_EXTENSIONS, MAX_FILE_SIZE, PORT, UPLOAD_TOKEN, HOST

app = Flask(__name__, template_folder="utils")


@app.route("/")
@app.route("/<path:requested_path>")
def list_directory(requested_path=""):
    requested_path = urllib.parse.unquote(requested_path)
    full_path = os.path.join(os.getcwd(), requested_path)

    if check(full_path):
        abort(404, "Files or Directory not found or missing")

    if os.path.isdir(full_path):
        try:
            file_list = os.listdir(full_path)
        except OSError:
            abort(404, "Files or Directory not found or missing")

        file_list.sort(key=lambda a: a.lower())
        rows = ""

        if requested_path:
            parent_path = os.path.normpath(os.path.join("/", requested_path, ".."))
            parent_link = f'<a href="{urllib.parse.quote(parent_path)}">..</a>'
            rows += f"""
                <tr>
                    <td colspan="3">{parent_link}</td>
                </tr>
            """

        for file_name in file_list:
            full_file_path = os.path.join(full_path, file_name)

            if check(full_file_path):
                continue

            display_name = (
                file_name + "/" if os.path.isdir(full_file_path) else file_name
            )
            link = urllib.parse.quote(os.path.join("/", requested_path, file_name))
            file_size = format_size(os.path.getsize(full_file_path))
            last_modified = datetime.fromtimestamp(
                os.path.getmtime(full_file_path)
            ).strftime("%Y-%m-%d %H:%M:%S")

            rows += f"""
                <tr>
                    <td class="file-name"><a href="{link}">{display_name}</a></td>
                    <td class="file-size">{file_size if not os.path.isdir(full_file_path) else '-'}</td>
                    <td class="date">{last_modified}</td>
                </tr>
            """

        if requested_path:
            segments = requested_path.split("/")
            last_segment = segments[-1]
            prefix = "../" * (len(segments) - 1)
            directory = f"../{prefix}{last_segment}" + "/"
        else:
            directory = f"{request.host}"

        hostname = request.host
        current_year = datetime.now().year
        content = render_template(
            "template.html",
            directory=directory,
            rows=rows,
            year=current_year,
            host=hostname,
        )
        encoded = content.encode("utf-8", "surrogateescape")
        return gzip_response(app, encoded)

    elif os.path.isfile(full_path):
        return cust_send_file(full_path)
        # return send_file(full_path, as_attachment=True, etag=False)
    else:
        abort(404, "Files or Directory not found or missing")


@app.route("/upload", methods=["POST"])
def upload_file():
    token = request.headers.get("X-Upload-Token")
    if token != UPLOAD_TOKEN:
        return {"error": "Unauthorized"}, 401

    if "file" not in request.files:
        return {"error": "No file to upload"}, 400

    file = request.files["file"]

    if file.filename == "" or not allowed_file(file.filename):
        return {"error": "No selected file or file type not allowed"}, 400

    if file.content_length > MAX_FILE_SIZE:
        return {"error": "File too large"}, 400

    target_path = request.args.get("path", "")

    current_directory = os.getcwd()
    full_target_path = os.path.join(current_directory, target_path)
    try:
        os.makedirs(full_target_path, exist_ok=True)
    except OSError as e:
        return {"error": f"Error creating directory: {str(e)}"}, 500

    safe_filename = secure_filename(file.filename)
    file.save(os.path.join(full_target_path, safe_filename))

    return {"success": f"File uploaded to {os.path.join(target_path, safe_filename)}"}


if __name__ == "__main__":
    app.run(host=HOST, port=PORT)
