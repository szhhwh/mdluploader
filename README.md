# MDLUploader

一个用于自动检测 Markdown 文件中的图像并将其上传到云存储的CLI工具。

## 功能特点

- 高效解析 Markdown 文件以提取图像引用
- 同时处理绝对和相对图像路径
- 使用MD5哈希比较识别新增、缺失和修改的图像
- 仅上传必要的文件以节省带宽和时间
- 使用并行化加速文件处理
- 目前支持AWS S3和兼容服务

## 安装

```bash
# 克隆仓库
git clone https://github.com/szhhwh/mdluploader.git
cd mdluploader

# 构建项目
cargo build --release

# 可执行文件将位于 target/release/mdluploader
```

## 使用方法

```bash
# 将 Markdown 文件中的图像上传到S3
mdluploader upload /path/to/markdown/folder \
  --bucket my-bucket \
  --ak ACCESS_KEY \
  --sk SECRET_KEY \
  --region us-west-1 \
  --endpoint https://s3.us-west-1.amazonaws.com \
  --remote-root remote_dir \
  --domain https://www.example.com \
  --depth 5
```

### 命令

#### `upload`

将Markdown文件中引用的图像上传到云存储。

参数:
- `path`: 包含要扫描的Markdown文件的目录（必需）
- `-b, --bucket`: S3 存储桶名称（必需）
- `-a, --ak`: S3访问密钥（必需）
- `-s, --sk`: S3密钥（必需）
- `-g, --region`: S3区域（必需）
- `-e, --endpoint`: S3端点URL（必需）
- `-r, --remote-root`: S3远端目录（可选）
- `-d, --domain`: S3自定义访问域名（必需）
- `--depth`: 扫描的最大目录深度（默认值：10）

## 工作原理

1. 递归扫描提供的目录中的Markdown文件
2. 从Markdown文件中提取所有图像引用
3. 解析图像路径（绝对和相对路径）
4. 计算所有本地图像的MD5校验和
5. 从云端获取现有文件列表及其校验和
6. 比较本地和远程文件以确定需要上传或替换的内容
7. 执行必要的上传操作

## 依赖项

- Rust 1.68+
- OpenDAL 用于云存储操作
- Tokio 用于异步运行时
- Rayon 用于并行化处理
- Clap 用于命令行解析
