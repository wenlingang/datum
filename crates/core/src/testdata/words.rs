//! 内置词表(英文 / 简体中文):人名、地名、公司、文本片段。
//!
//! 不引入 faker 一类的大依赖:测试数据只要「看起来像」,几十到上百个词组合起来足够丰富,
//! 而且词表随代码固定,同一个种子在各版本上生成同样的内容。

pub const EN_FIRST: &[&str] = &[
    "James", "Mary", "John", "Patricia", "Robert", "Jennifer", "Michael", "Linda", "William", "Elizabeth",
    "David", "Barbara", "Richard", "Susan", "Joseph", "Jessica", "Thomas", "Sarah", "Charles", "Karen",
    "Daniel", "Nancy", "Matthew", "Lisa", "Anthony", "Betty", "Mark", "Sandra", "Steven", "Ashley",
    "Paul", "Emily", "Andrew", "Donna", "Joshua", "Michelle", "Kevin", "Carol", "Brian", "Amanda",
    "George", "Melissa", "Edward", "Deborah", "Ryan", "Laura", "Jacob", "Rachel", "Nicholas", "Olivia",
    "Ethan", "Sophia", "Noah", "Emma", "Liam", "Ava", "Lucas", "Mia", "Henry", "Chloe",
];

pub const EN_LAST: &[&str] = &[
    "Smith", "Johnson", "Williams", "Brown", "Jones", "Garcia", "Miller", "Davis", "Rodriguez", "Martinez",
    "Hernandez", "Lopez", "Gonzalez", "Wilson", "Anderson", "Thomas", "Taylor", "Moore", "Jackson", "Martin",
    "Lee", "Perez", "Thompson", "White", "Harris", "Sanchez", "Clark", "Ramirez", "Lewis", "Robinson",
    "Walker", "Young", "Allen", "King", "Wright", "Scott", "Torres", "Nguyen", "Hill", "Flores",
    "Green", "Adams", "Nelson", "Baker", "Hall", "Rivera", "Campbell", "Mitchell", "Carter", "Roberts",
];

pub const EN_CITY: &[&str] = &[
    "New York", "Los Angeles", "Chicago", "Houston", "Phoenix", "Philadelphia", "San Antonio", "San Diego",
    "Dallas", "Austin", "Seattle", "Denver", "Boston", "Portland", "Atlanta", "Miami", "Toronto", "Vancouver",
    "London", "Manchester", "Dublin", "Sydney", "Melbourne", "Auckland", "Berlin", "Paris", "Amsterdam",
    "Madrid", "Lisbon", "Stockholm", "Singapore", "Tokyo",
];

pub const EN_COUNTRY: &[&str] = &[
    "United States", "Canada", "United Kingdom", "Ireland", "Australia", "New Zealand", "Germany", "France",
    "Netherlands", "Spain", "Portugal", "Sweden", "Singapore", "Japan", "Brazil", "Mexico", "India", "Kenya",
];

pub const EN_STREET: &[&str] = &[
    "Main St", "Oak Ave", "Maple Dr", "Cedar Ln", "Pine St", "Elm St", "Washington Blvd", "Lake Rd",
    "Hill St", "Park Ave", "Sunset Blvd", "River Rd", "Church St", "High St", "Market St", "Broadway",
];

pub const EN_COMPANY_PREFIX: &[&str] = &[
    "Acme", "Globex", "Initech", "Umbrella", "Stark", "Wayne", "Wonka", "Hooli", "Vandelay", "Soylent",
    "Northwind", "Contoso", "Fabrikam", "Tailspin", "Blue Yonder", "Summit", "Pioneer", "Evergreen",
    "Silverline", "Bright", "Atlas", "Nimbus", "Redwood", "Harbor",
];

pub const EN_COMPANY_SUFFIX: &[&str] = &["Inc", "LLC", "Ltd", "Group", "Labs", "Systems", "Partners", "Co"];

pub const EN_WORDS: &[&str] = &[
    "lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit", "sed", "do", "eiusmod",
    "tempor", "incididunt", "ut", "labore", "et", "dolore", "magna", "aliqua", "enim", "ad", "minim",
    "veniam", "quis", "nostrud", "exercitation", "ullamco", "laboris", "nisi", "aliquip", "ex", "ea",
    "commodo", "consequat", "duis", "aute", "irure", "in", "reprehenderit", "voluptate", "velit", "esse",
    "cillum", "fugiat", "nulla", "pariatur", "excepteur", "sint", "occaecat", "cupidatat", "non", "proident",
];

pub const EN_TITLE_WORDS: &[&str] = &[
    "Ultimate", "Quick", "Modern", "Essential", "Practical", "Hidden", "Simple", "Complete", "Smart", "Better",
    "Guide", "Notes", "Report", "Plan", "Review", "Update", "Story", "Checklist", "Overview", "Handbook",
    "Coffee", "Garden", "Travel", "Design", "Data", "Kitchen", "Music", "Market", "Project", "Weekend",
];

pub const EMAIL_DOMAINS: &[&str] = &["example.com", "example.org", "example.net", "mail.test", "corp.test"];

pub const URL_PATHS: &[&str] = &["about", "blog", "products", "docs", "news", "help", "pricing", "team"];

pub const ZH_SURNAME: &[&str] = &[
    "王", "李", "张", "刘", "陈", "杨", "黄", "赵", "吴", "周", "徐", "孙", "马", "朱", "胡", "郭", "何", "林",
    "罗", "高", "郑", "梁", "谢", "宋", "唐", "许", "韩", "冯", "邓", "曹", "彭", "曾", "肖", "田", "董", "潘",
    "袁", "蔡", "蒋", "余", "于", "杜", "叶", "程", "魏", "苏", "吕", "丁", "任", "沈", "欧阳", "司马",
];

pub const ZH_GIVEN: &[&str] = &[
    "伟", "芳", "娜", "敏", "静", "丽", "强", "磊", "军", "洋", "勇", "艳", "杰", "娟", "涛", "明", "超", "秀英",
    "霞", "平", "刚", "桂英", "华", "建国", "志强", "晓东", "子涵", "梓轩", "欣怡", "浩然", "雨桐", "宇航",
    "思远", "若曦", "一诺", "嘉怡", "俊杰", "佳琪", "文博", "诗涵", "天佑", "梦瑶", "晨阳", "可馨", "志明", "海燕",
];

pub const ZH_CITY: &[&str] = &[
    "北京", "上海", "广州", "深圳", "杭州", "南京", "成都", "重庆", "武汉", "西安", "苏州", "天津", "长沙",
    "郑州", "青岛", "厦门", "宁波", "合肥", "福州", "昆明", "大连", "沈阳", "济南", "无锡", "东莞", "佛山",
];

pub const ZH_PROVINCE: &[&str] = &[
    "北京市", "上海市", "广东省", "浙江省", "江苏省", "四川省", "重庆市", "湖北省", "陕西省", "天津市",
    "湖南省", "河南省", "山东省", "福建省", "安徽省", "云南省", "辽宁省",
];

pub const ZH_DISTRICT: &[&str] = &["朝阳区", "海淀区", "浦东新区", "天河区", "南山区", "西湖区", "鼓楼区", "武侯区", "江北区", "高新区"];

pub const ZH_STREET: &[&str] = &[
    "中山路", "人民路", "解放路", "建设路", "和平路", "长江路", "黄河路", "文化路", "胜利路", "新华路",
    "青年路", "滨江大道", "科技园路", "学院路", "复兴路", "东风路",
];

pub const ZH_COUNTRY: &[&str] = &["中国", "日本", "韩国", "新加坡", "美国", "英国", "德国", "法国", "澳大利亚", "加拿大"];

pub const ZH_COMPANY_WORD: &[&str] = &[
    "华信", "创新", "腾飞", "鼎盛", "宏图", "博远", "盛世", "众诚", "瑞丰", "启航", "云帆", "智联", "汇通",
    "恒达", "嘉禾", "新锐", "长青", "天成",
];

pub const ZH_COMPANY_KIND: &[&str] = &["科技", "信息技术", "网络科技", "贸易", "电子商务", "文化传媒", "咨询", "实业"];

pub const ZH_PHRASES: &[&str] = &[
    "今天天气不错", "这个功能很好用", "我们下周再讨论", "请尽快处理", "已经发货了", "感谢您的耐心等待",
    "数据已同步", "需要进一步确认", "价格比较合理", "质量非常好", "物流速度很快", "服务态度热情",
    "下次还会再来", "包装完好无损", "建议增加更多选项", "整体体验满意", "细节还可以改进", "按计划推进",
    "客户反馈积极", "已安排专人跟进",
];

pub const ZH_TITLE_WORDS: &[&str] = &[
    "新品", "年度", "精选", "实用", "入门", "进阶", "完整", "快速", "指南", "笔记", "报告", "计划", "回顾",
    "清单", "攻略", "手册", "咖啡", "旅行", "设计", "数据", "厨房", "音乐", "市场", "项目",
];

/// 手机号段前三位(中国大陆)。
pub const ZH_MOBILE_PREFIX: &[&str] = &[
    "130", "131", "132", "133", "135", "136", "137", "138", "139", "150", "151", "152", "155", "158", "159",
    "166", "175", "176", "177", "180", "181", "185", "186", "187", "188", "189", "191", "198", "199",
];
